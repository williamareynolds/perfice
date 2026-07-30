//! Internal gRPC surface.
//!
//! Consumed by the gateway (to resolve bearer tokens) and by sync and
//! integration (for session lists and timezones). Not exposed publicly, and
//! deliberately unauthenticated -- it lives on a private port, exactly as in Go.

use perfice_proto::user_service_server::{UserService, UserServiceServer};
use perfice_proto::{
    AuthenticationRequest, AuthenticationResponse, GetSessionsRequest, GetSessionsResponse,
    GetUserTimeZoneRequest, GetUserTimeZoneResponse, GetUsersTimeZonesRequest,
    GetUsersTimeZonesResponse, Session as ProtoSession, SuccessfulAuthenticationResponse,
    authentication_response,
};
use tonic::{Request, Response, Status};

use crate::service::AuthService;
use crate::session::SessionService;

pub struct UserGrpc {
    auth: AuthService,
    sessions: SessionService,
}

impl UserGrpc {
    pub fn server(auth: AuthService, sessions: SessionService) -> UserServiceServer<Self> {
        UserServiceServer::new(Self { auth, sessions })
    }
}

#[tonic::async_trait]
impl UserService for UserGrpc {
    /// Note the response shape: a rejected token is a *successful* RPC carrying
    /// an error string, not a gRPC error. Callers must check which arm of the
    /// oneof came back.
    async fn authenticate(
        &self,
        request: Request<AuthenticationRequest>,
    ) -> Result<Response<AuthenticationResponse>, Status> {
        let token = request.into_inner().token;

        let result = match self.sessions.authenticate(&token).await {
            Ok((user_id, session_id)) => {
                authentication_response::Result::Auth(SuccessfulAuthenticationResponse {
                    user_id,
                    session_id,
                })
            }
            Err(_) => authentication_response::Result::Error("Invalid token".to_owned()),
        };

        Ok(Response::new(AuthenticationResponse {
            result: Some(result),
        }))
    }

    async fn get_sessions(
        &self,
        request: Request<GetSessionsRequest>,
    ) -> Result<Response<GetSessionsResponse>, Status> {
        let user_id = request.into_inner().user_id;

        let sessions = self
            .sessions
            .sessions_for_user(&user_id)
            .await
            .map_err(|err| Status::internal(err.to_string()))?;

        Ok(Response::new(GetSessionsResponse {
            sessions: sessions
                .into_iter()
                .map(|session| ProtoSession {
                    id: session.id,
                    user_id: session.user,
                    expiry: session.expiry,
                })
                .collect(),
        }))
    }

    async fn get_user_time_zone(
        &self,
        request: Request<GetUserTimeZoneRequest>,
    ) -> Result<Response<GetUserTimeZoneResponse>, Status> {
        let user_id = request.into_inner().user_id;

        let timezone = self
            .auth
            .timezone(&user_id)
            .await
            .map_err(|err| Status::internal(err.to_string()))?;

        Ok(Response::new(GetUserTimeZoneResponse { timezone }))
    }

    async fn get_users_time_zones(
        &self,
        request: Request<GetUsersTimeZonesRequest>,
    ) -> Result<Response<GetUsersTimeZonesResponse>, Status> {
        let user_ids = request.into_inner().user_ids;

        let timezones = self
            .auth
            .timezones(&user_ids)
            .await
            .map_err(|err| Status::internal(err.to_string()))?;

        Ok(Response::new(GetUsersTimeZonesResponse { timezones }))
    }
}
