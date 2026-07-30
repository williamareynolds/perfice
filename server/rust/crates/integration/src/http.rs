//! HTTP surface.
//!
//! Every route requires the gateway secret. Two of them are additionally
//! *unauthenticated* in the user sense, and both for the same reason: the
//! caller is a third party that cannot hold one of our bearer tokens. A
//! provider posting to a webhook proves itself with the token in the path, and
//! a provider redirecting a browser to the OAuth callback proves itself with
//! the `state` it was given.

use axum::body::Bytes;
use axum::extract::{FromRef, Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use mongodb::bson::oid::ObjectId;
use perfice_common::error::{ApiError, ApiResult};
use perfice_common::identity::{InternalSecret, UserIdentity, check_internal_secret};
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;

use crate::model::{
    AcknowledgeUpdatesRequest, CreateIntegrationRequest, IntegrationEntityResponse,
    IntegrationTypeResponse, IntegrationUpdateResponse, UpdateIntegrationRequest,
    UserIntegrationResponse,
};
use crate::paths::bson_to_json;
use crate::service::{IntegrationService, WebhookError};

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<IntegrationService>,
    pub secret: InternalSecret,
}

impl FromRef<AppState> for InternalSecret {
    fn from_ref(state: &AppState) -> Self {
        state.secret.clone()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/integrations/push/{token}", post(webhook))
        // Both spellings, because the gateway forwards the path verbatim and
        // the client sends the trailing slash.
        .route("/integrations", get(list).post(create))
        .route("/integrations/", get(list).post(create))
        .route(
            "/integrations/{id}",
            axum::routing::put(update).delete(delete),
        )
        .route("/integrations/{id}/historical", post(historical))
        .route("/integrationTypes", get(integration_types))
        .route("/integrationTypes/", get(integration_types))
        .route(
            "/integrationTypes/{integration_type}/authenticated",
            get(authentication_status),
        )
        .route(
            "/integrationTypes/{integration_type}/redirect",
            get(authorization_redirect),
        )
        .route(
            "/integrationTypes/{integration_type}/callback",
            get(callback),
        )
        .route("/updates", get(updates))
        .route("/updates/ack", post(acknowledge_updates))
        .with_state(state)
}

// --- user integrations ------------------------------------------------------

async fn list(
    State(state): State<AppState>,
    identity: UserIdentity,
) -> ApiResult<impl IntoResponse> {
    let integrations = state.service.list(&identity.user_id).await?;

    Ok(Json(
        integrations
            .into_iter()
            .map(UserIntegrationResponse::from)
            .collect::<Vec<_>>(),
    ))
}

async fn create(
    State(state): State<AppState>,
    identity: UserIdentity,
    Json(request): Json<CreateIntegrationRequest>,
) -> ApiResult<impl IntoResponse> {
    // Every field is required. Absent ones are rejected here rather than
    // producing an integration that can never resolve a definition.
    let (Some(integration_type), Some(entity_type), Some(form_id), Some(fields), Some(options)) = (
        request.integration_type,
        request.entity_type,
        request.form_id,
        request.fields,
        request.options,
    ) else {
        return Err(ApiError::bad_request("Missing required field"));
    };

    let created = state
        .service
        .create(
            &identity.user_id,
            &integration_type,
            &entity_type,
            &form_id,
            fields,
            options,
        )
        .await?;

    // Asking for a provider that does not exist is the caller's mistake, not a
    // fault.
    let created =
        created.ok_or_else(|| ApiError::bad_request("Unknown integration or entity type"))?;

    Ok(Json(created))
}

async fn update(
    State(state): State<AppState>,
    identity: UserIdentity,
    Path(id): Path<String>,
    Json(request): Json<UpdateIntegrationRequest>,
) -> ApiResult<impl IntoResponse> {
    let (Some(fields), Some(options)) = (request.fields, request.options) else {
        return Err(ApiError::bad_request("Missing required field"));
    };

    let updated = state
        .service
        .update(&id, &identity.user_id, fields, options)
        .await?
        .ok_or(ApiError::NotFound)?;

    Ok(Json(updated))
}

async fn delete(
    State(state): State<AppState>,
    identity: UserIdentity,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    state.service.delete(&id, &identity.user_id).await?;
    Ok(StatusCode::OK)
}

async fn historical(
    State(state): State<AppState>,
    identity: UserIdentity,
    Path(id): Path<String>,
) -> ApiResult<impl IntoResponse> {
    // A failed backfill answers 404, matching Go: the caller cannot tell a
    // missing integration from a provider that would not co-operate, and
    // neither is actionable beyond "try again".
    state
        .service
        .fetch_historical(&id, &identity.user_id)
        .await
        .map_err(|err| {
            tracing::error!(integration = %id, error = ?err, "historical fetch failed");
            ApiError::NotFound
        })?;

    Ok(StatusCode::OK)
}

// --- provider types ---------------------------------------------------------

async fn integration_types(
    State(state): State<AppState>,
    identity: UserIdentity,
) -> ApiResult<impl IntoResponse> {
    let definitions = state.service.definitions();
    let credentials = state
        .service
        .auth()
        .credentials_by_user(&identity.user_id)
        .await?;

    let mut response = Vec::new();
    for definition in definitions.types() {
        // A provider with no entities has nothing the user could connect, so
        // it is hidden rather than shown as an empty card.
        let Some(entities) = definitions.entities_for(&definition.integration_type) else {
            continue;
        };

        let entities = entities
            .iter()
            .map(|entity| IntegrationEntityResponse {
                name: entity.name.clone(),
                entity_type: entity.entity_type.clone(),
                fields: entity
                    .fields
                    .iter()
                    .map(|(key, field)| (key.clone(), field.name.clone()))
                    .collect::<HashMap<_, _>>(),
                options: entity.options.clone(),
                historical: entity.history.is_some(),
            })
            .collect();

        let authenticated = definition.authentication.is_none()
            || credentials
                .iter()
                .any(|credential| credential.integration_type == definition.integration_type);

        response.push(IntegrationTypeResponse {
            integration_type: definition.integration_type.clone(),
            authenticated,
            name: definition.name.clone(),
            logo: definition.logo.clone(),
            entities,
        });
    }

    Ok(Json(response))
}

async fn authentication_status(
    State(state): State<AppState>,
    identity: UserIdentity,
    Path(integration_type): Path<String>,
) -> ApiResult<impl IntoResponse> {
    let authenticated = state
        .service
        .auth()
        .is_authenticated(
            state.service.definitions(),
            &identity.user_id,
            &integration_type,
        )
        .await?;

    if !authenticated {
        return Err(ApiError::NotFound);
    }

    Ok(StatusCode::OK)
}

async fn authorization_redirect(
    State(state): State<AppState>,
    identity: UserIdentity,
    Path(integration_type): Path<String>,
) -> ApiResult<impl IntoResponse> {
    // The URL is returned as text for the client to navigate to, rather than
    // sent as a 302: the caller is a fetch from the app, not a browser
    // following links.
    state
        .service
        .auth()
        .authorization_url(&integration_type, &identity.user_id)
        .ok_or(ApiError::NotFound)
}

#[derive(Debug, Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: String,
    #[serde(default)]
    state: String,
}

/// The provider's redirect back to us.
///
/// Unauthenticated by necessity -- it is a browser navigation triggered by the
/// provider. The `state` parameter is what identifies the user, and it is
/// single-use.
async fn callback(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(integration_type): Path<String>,
    Query(query): Query<CallbackQuery>,
) -> ApiResult<impl IntoResponse> {
    check_internal_secret(&headers, &state.secret)?;

    state
        .service
        .auth()
        .handle_callback(&integration_type, &query.code, &query.state)
        .await?;

    Ok("You have successfully authenticated and can now close this window")
}

// --- updates ----------------------------------------------------------------

async fn updates(
    State(state): State<AppState>,
    identity: UserIdentity,
) -> ApiResult<impl IntoResponse> {
    let updates = state.service.updates(&identity.user_id).await?;

    Ok(Json(
        updates
            .into_iter()
            .map(|update| IntegrationUpdateResponse {
                id: update.id.to_hex(),
                integration_id: update.integration_id,
                identifier: update.identifier,
                // Stored as BSON, answered as plain JSON.
                data: update.data.map(|data| bson_to_json(&data.into())),
                timestamp: update.timestamp,
            })
            .collect::<Vec<_>>(),
    ))
}

async fn acknowledge_updates(
    State(state): State<AppState>,
    identity: UserIdentity,
    Json(request): Json<AcknowledgeUpdatesRequest>,
) -> ApiResult<impl IntoResponse> {
    let ids = request
        .updates
        .iter()
        .map(|id| id.parse::<ObjectId>())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| ApiError::bad_request("Invalid update id"))?;

    state
        .service
        .acknowledge_updates(&identity.user_id, &ids)
        .await?;

    Ok(StatusCode::OK)
}

// --- webhooks ---------------------------------------------------------------

/// A payload pushed by a provider.
///
/// The path token is the whole credential, so it is treated as one: an unknown
/// token is a 404 and never reveals whether it merely belongs to someone else.
async fn webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(token): Path<String>,
    body: Bytes,
) -> ApiResult<impl IntoResponse> {
    check_internal_secret(&headers, &state.secret)?;

    match state.service.handle_webhook(&token, &body).await {
        Ok(()) => Ok(StatusCode::OK.into_response()),
        // Both of these are the caller's problem, and providers retry on 5xx --
        // answering 500 to a permanently bad request means retrying forever.
        Err(WebhookError::UnknownToken) => Err(ApiError::NotFound),
        Err(WebhookError::Malformed) => Err(ApiError::bad_request("Malformed payload")),
        Err(WebhookError::Other(err)) => Err(ApiError::Internal(err)),
    }
}
