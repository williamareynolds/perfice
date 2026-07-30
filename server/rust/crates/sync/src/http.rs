//! HTTP surface. Every route requires the gateway secret and both identity
//! headers, enforced by the `Identity` extractor.

use axum::extract::{FromRef, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use perfice_common::error::{ApiError, ApiResult};
use perfice_common::identity::{Identity, InternalSecret};
use std::collections::HashMap;

use crate::model::{
    AckRequest, FullPullRequest, FullPullResponse, KeyResponse, OP_CREATE, OP_DELETE, OP_FULL_SYNC,
    OP_PUT, PullResponse, PushRequest, PushResponse, SaltResponse, SetKeyRequest,
    is_known_entity_type,
};
use crate::service::SyncService;

#[derive(Clone)]
pub struct AppState {
    pub sync: SyncService,
    pub secret: InternalSecret,
}

impl FromRef<AppState> for InternalSecret {
    fn from_ref(state: &AppState) -> Self {
        state.secret.clone()
    }
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/push", post(push))
        .route("/pull", post(pull))
        .route("/ack", post(ack))
        .route("/fullPull", post(full_pull))
        .route("/key", get(get_key).put(set_key))
        .route("/salt", get(salt))
        .with_state(state)
}

/// Rejects anything that would be applied incorrectly rather than discovering
/// it mid-transaction.
///
/// This runs before any write: a batch containing one bad update is refused
/// whole, so the caller never sees a partially applied push reported as
/// success.
fn validate(request: &PushRequest) -> Result<(), ApiError> {
    for update in &request.updates {
        if !is_known_entity_type(&update.entity_type) {
            return Err(ApiError::bad_request("Invalid entity type"));
        }

        if !matches!(
            update.operation.as_str(),
            OP_CREATE | OP_PUT | OP_DELETE | OP_FULL_SYNC
        ) {
            return Err(ApiError::bad_request("Invalid operation"));
        }

        if update.timestamp == 0 {
            return Err(ApiError::bad_request("Timestamp is required"));
        }

        if uuid_like(&update.id).is_none() {
            return Err(ApiError::bad_request("Update id must be a uuid"));
        }

        // Only a delete may omit the payload.
        if update.operation != OP_DELETE
            && update.entities.iter().any(|entity| entity.data.is_none())
        {
            return Err(ApiError::bad_request(format!(
                "Entity data is required for {}",
                update.operation
            )));
        }
    }

    Ok(())
}

/// Accepts the canonical hyphenated UUID form the client emits.
fn uuid_like(value: &str) -> Option<()> {
    let groups = [8, 4, 4, 4, 12];
    let mut parts = value.split('-');

    for expected in groups {
        let part = parts.next()?;
        if part.len() != expected || !part.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
    }

    parts.next().is_none().then_some(())
}

async fn push(
    State(state): State<AppState>,
    identity: Identity,
    Json(request): Json<PushRequest>,
) -> ApiResult<impl IntoResponse> {
    validate(&request)?;

    let ack = state
        .sync
        .push(&identity.user_id, &identity.session_id, request.updates)
        .await?;

    Ok(Json(PushResponse { ack }))
}

async fn pull(State(state): State<AppState>, identity: Identity) -> ApiResult<impl IntoResponse> {
    let key = state.sync.key(&identity.user_id).await?;

    // Updates are withheld until the user has a verification key: without one
    // a receiving device could not decrypt them anyway.
    let updates = match key {
        None => Vec::new(),
        Some(_) => state
            .sync
            .pending(&identity.user_id, &identity.session_id)
            .await?
            .into_iter()
            .map(Into::into)
            .collect(),
    };

    Ok(Json(PullResponse { key, updates }))
}

async fn ack(
    State(state): State<AppState>,
    identity: Identity,
    Json(request): Json<AckRequest>,
) -> ApiResult<impl IntoResponse> {
    state
        .sync
        .ack(&identity.user_id, &identity.session_id, &request.updates)
        .await?;
    Ok(StatusCode::OK)
}

async fn full_pull(
    State(state): State<AppState>,
    identity: Identity,
    Json(request): Json<FullPullRequest>,
) -> ApiResult<impl IntoResponse> {
    // Validated up front so an unknown type is a 400 rather than surfacing
    // from the service layer as an internal error.
    if let Some(types) = &request.entity_types
        && let Some(unknown) = types.iter().find(|t| !is_known_entity_type(t)) {
            tracing::debug!(%unknown, "full pull requested an unknown entity type");
            return Err(ApiError::bad_request("Invalid entity type"));
        }

    let entities = state
        .sync
        .full_pull(
            &identity.user_id,
            &identity.session_id,
            request.entity_types,
        )
        .await?;

    let entities: HashMap<_, _> = entities
        .into_iter()
        .map(|(entity_type, stored)| {
            (
                entity_type,
                stored.into_iter().map(Into::into).collect::<Vec<_>>(),
            )
        })
        .collect();

    Ok(Json(FullPullResponse { entities }))
}

async fn get_key(
    State(state): State<AppState>,
    identity: Identity,
) -> ApiResult<impl IntoResponse> {
    let key = state.sync.key(&identity.user_id).await?;
    Ok(Json(KeyResponse { key }))
}

async fn set_key(
    State(state): State<AppState>,
    identity: Identity,
    Json(request): Json<SetKeyRequest>,
) -> ApiResult<impl IntoResponse> {
    // An empty key is a distinct, reachable state that would silently unblock
    // replication, so it is rejected rather than stored.
    let key = request.key.filter(|key| !key.is_empty());
    let key = key.ok_or_else(|| ApiError::bad_request("Key is required"))?;

    state.sync.set_key(&identity.user_id, key).await?;
    Ok(StatusCode::OK)
}

async fn salt(State(state): State<AppState>, identity: Identity) -> ApiResult<impl IntoResponse> {
    let salt = state.sync.salt(&identity.user_id).await?;
    Ok(Json(SaltResponse { salt }))
}

#[cfg(test)]
mod tests {
    use super::uuid_like;

    #[test]
    fn accepts_canonical_uuids() {
        assert!(uuid_like("c288c96d-b5d8-4501-9f8d-c0e127459e8a").is_some());
    }

    #[test]
    fn rejects_other_shapes() {
        for value in [
            "",
            "not-a-uuid",
            "c288c96db5d845019f8dc0e127459e8a",
            "c288c96d-b5d8-4501-9f8d-c0e127459e8",
            "c288c96d-b5d8-4501-9f8d-c0e127459e8a-extra",
            "g288c96d-b5d8-4501-9f8d-c0e127459e8a",
        ] {
            assert!(uuid_like(value).is_none(), "{value:?} was accepted");
        }
    }
}
