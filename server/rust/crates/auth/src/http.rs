//! Public HTTP surface.
//!
//! Reached only through the gateway, which is enforced by the internal-secret
//! guard applied to the whole router.

use axum::extract::{FromRef, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post, put};
use axum::{Json, Router};
use perfice_common::error::{ApiError, ApiResult};
use perfice_common::identity::{InternalSecret, check_internal_secret};
use serde::{Deserialize, Serialize};

use crate::model::Session;
use crate::service::{AuthError, AuthService};
use crate::session::{SessionError, SessionService};
use crate::validation;

#[derive(Clone)]
pub struct AppState {
    pub auth: AuthService,
    pub sessions: SessionService,
    pub secret: InternalSecret,
}

impl FromRef<AppState> for InternalSecret {
    fn from_ref(state: &AppState) -> Self {
        state.secret.clone()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/register", post(register))
        .route("/login", post(login))
        .route("/refresh", post(refresh))
        .route("/timezone", put(set_timezone))
        .route("/me", get(me))
        .route("/delete", post(delete_account))
        .route("/logout", post(logout))
        .route("/feedback", post(feedback))
        // Mail-dependent flows. The gateway routes to all of these, so they
        // must exist and answer as they do in Go rather than 404.
        .route("/confirm/{token}", get(mail_disabled))
        .route("/resetInit", post(mail_disabled))
        .route("/reset", post(mail_disabled))
        .route("/reset/{token}", get(mail_disabled))
        .route("/resendConfirm", post(mail_disabled))
        .with_state(state)
}

/// Email confirmation and password reset need a mail service; none is
/// configured, and Go answers 400 on every one of these paths in that state
/// (the token never exists, so it never parses or never resolves).
///
/// Preserved rather than 404'd so the observable behaviour is unchanged. When
/// the mail flows are ported these become real handlers.
async fn mail_disabled(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;
    Err::<StatusCode, _>(ApiError::bad_request("Invalid token"))
}

// --- request/response bodies ------------------------------------------------

#[derive(Deserialize)]
struct CredentialsRequest {
    #[serde(default)]
    email: String,
    #[serde(default)]
    password: String,
}

#[derive(Deserialize)]
struct RefreshRequest {
    #[serde(rename = "accessToken", default)]
    access_token: String,
    #[serde(rename = "refreshToken", default)]
    refresh_token: String,
}

#[derive(Deserialize)]
struct TimezoneRequest {
    #[serde(default)]
    timezone: String,
}

#[derive(Serialize)]
struct SessionResponse {
    #[serde(rename = "accessToken")]
    access_token: String,
    #[serde(rename = "refreshToken")]
    refresh_token: String,
}

impl From<Session> for SessionResponse {
    fn from(session: Session) -> Self {
        Self {
            access_token: session.access_token,
            refresh_token: session.refresh_token,
        }
    }
}

#[derive(Serialize)]
struct MeResponse {
    id: String,
    timezone: String,
}

// --- authentication ---------------------------------------------------------

/// Resolves the caller from the bearer token, rejecting revoked sessions.
///
/// The gateway has already validated the token, but this service is also
/// reachable directly (behind the internal secret) and its Go counterpart
/// applies its own JWT middleware, so the check is repeated here rather than
/// trusted from upstream.
async fn authenticate(state: &AppState, headers: &HeaderMap) -> Result<(String, String), ApiError> {
    let token = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .ok_or(ApiError::Unauthorized)?;

    state
        .sessions
        .authenticate(token)
        .await
        .map_err(|err| match err {
            SessionError::InvalidSession => ApiError::Unauthorized,
            SessionError::Internal(err) => ApiError::Internal(err),
        })
}

fn guard(headers: &HeaderMap, state: &AppState) -> Result<(), ApiError> {
    check_internal_secret(headers, &state.secret)
}

// --- handlers ---------------------------------------------------------------

async fn register(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CredentialsRequest>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;
    let Json(request) = body.map_err(|_| ApiError::bad_request("malformed body"))?;

    let email = validation::sanitize_email(&request.email);
    validation::validate_credentials(&email, &request.password)
        .map_err(|err| ApiError::bad_request(err.message()))?;

    match state.auth.register(&email, &request.password).await {
        Ok(()) => Ok(StatusCode::OK),
        Err(AuthError::UserAlreadyExists) => Err(ApiError::bad_request("User already exists")),
        Err(AuthError::InvalidCredentials) => Err(ApiError::Unauthorized),
        Err(AuthError::Internal(err)) => Err(ApiError::Internal(err)),
    }
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<CredentialsRequest>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;
    let Json(request) = body.map_err(|_| ApiError::bad_request("malformed body"))?;

    let email = validation::sanitize_email(&request.email);
    match state.auth.login(&email, &request.password).await {
        Ok(session) => Ok(Json(SessionResponse::from(session))),
        // Identical response for an unknown address and a wrong password.
        Err(AuthError::InvalidCredentials) => Err(unauthorized_credentials()),
        Err(AuthError::UserAlreadyExists) => Err(unauthorized_credentials()),
        Err(AuthError::Internal(err)) => Err(ApiError::Internal(err)),
    }
}

/// Go answers a failed login with 401 and this exact body; the e2e suite
/// asserts the unknown-email and wrong-password responses are byte-identical.
fn unauthorized_credentials() -> ApiError {
    ApiError::WithBody(
        StatusCode::UNAUTHORIZED,
        "Invalid username or password".into(),
    )
}

async fn refresh(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<RefreshRequest>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;
    let Json(request) = body.map_err(|_| ApiError::bad_request("malformed body"))?;

    match state
        .sessions
        .refresh(&request.access_token, &request.refresh_token)
        .await
    {
        Ok(session) => Ok(Json(SessionResponse::from(session))),
        Err(SessionError::InvalidSession) => Err(ApiError::WithBody(
            StatusCode::UNAUTHORIZED,
            "Invalid session".into(),
        )),
        Err(SessionError::Internal(err)) => Err(ApiError::Internal(err)),
    }
}

async fn me(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;
    let (user_id, _) = authenticate(&state, &headers).await?;

    let timezone = state.auth.timezone(&user_id).await?;
    Ok(Json(MeResponse {
        id: user_id,
        timezone,
    }))
}

async fn set_timezone(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: Result<Json<TimezoneRequest>, axum::extract::rejection::JsonRejection>,
) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;
    let (user_id, _) = authenticate(&state, &headers).await?;
    let Json(request) = body.map_err(|_| ApiError::bad_request("malformed body"))?;

    if !validation::is_canonical_timezone(&request.timezone)
        || !validation::timezone_exists(&request.timezone)
    {
        return Err(ApiError::bad_request("Invalid timezone"));
    }

    state.auth.set_timezone(&user_id, &request.timezone).await?;
    Ok(StatusCode::OK)
}

async fn logout(State(state): State<AppState>, headers: HeaderMap) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;
    let (_, session_id) = authenticate(&state, &headers).await?;

    state.sessions.logout(&session_id).await?;
    Ok(StatusCode::OK)
}

async fn delete_account(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;
    let (user_id, _) = authenticate(&state, &headers).await?;

    state.auth.delete_user(&user_id).await?;
    Ok(StatusCode::OK)
}

/// Deliberately unauthenticated: people report problems they cannot log in to
/// describe. The abuse control is a size cap, not a credential.
async fn feedback(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: String,
) -> ApiResult<impl IntoResponse> {
    guard(&headers, &state)?;

    if body.is_empty() {
        return Err(ApiError::bad_request("Feedback is empty"));
    }

    if body.len() > validation::MAX_FEEDBACK_LENGTH {
        return Err(ApiError::bad_request("Feedback is too long"));
    }

    state.auth.insert_feedback(&body).await?;
    Ok(StatusCode::OK)
}
