use axum::http::StatusCode;
use uuid::Uuid;

use crate::Error;
use crate::models::NewChannel;
use crate::models::PermissionType;
use crate::models::{AuthSession, Backend};
use axum::Json;
use axum::Router;
use axum::extract::Path;
use axum::response::IntoResponse;
use axum::{routing::get, routing::post};
use axum_login::login_required;

pub fn router() -> Router {
    Router::new()
        .route("/{server_id}/list", get(get::list_channels))
        .route("/create", post(post::create_channel))
        .route_layer(login_required!(Backend))
}

mod post {

    use super::*;

    pub async fn create_channel(
        session: AuthSession,
        Json(new_channel): Json<NewChannel>,
    ) -> impl IntoResponse {
        let user = session.user.unwrap();
        let server_id = new_channel.server_id;
        let internal_err =
            |e: Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        let permission = if new_channel.hidden {
            PermissionType::CreateHiddenChannel
        } else {
            PermissionType::CreateChannel
        };
        match session
            .backend
            .has_permission(&user, server_id, permission, None)
        {
            Ok(true) => {}
            Ok(false) => {
                return (StatusCode::FORBIDDEN, "Permission denied").into_response();
            }
            Err(e) => {
                return internal_err(e);
            }
        }
        match session.backend.create_channel(&new_channel) {
            Ok(channel) => {
                let response = serde_json::to_string(&channel).unwrap();
                return (StatusCode::CREATED, response).into_response();
            }
            Err(e) => {
                return internal_err(e);
            }
        }
    }
}
mod get {
    use super::*;

    pub async fn list_channels(
        session: AuthSession,
        Path(server_id): Path<Uuid>,
    ) -> impl IntoResponse {
        let user = session.user.unwrap();
        let internal_err =
            |e: Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        let channels = match session.backend.list_channels(server_id) {
            Ok(channels) => channels,
            Err(e) => {
                return internal_err(e);
            }
        };
        let user_can_see_channel = match session.backend.has_permission(
            &user,
            server_id,
            PermissionType::ListChannels,
            None,
        ) {
            Ok(can_see) => can_see,
            Err(e) => {
                return internal_err(e);
            }
        };
        let user_can_see_hidden_channels = match session.backend.has_permission(
            &user,
            server_id,
            PermissionType::ListHiddenChannels,
            None,
        ) {
            Ok(can_see) => can_see,
            Err(e) => {
                return internal_err(e);
            }
        };
        let channels = channels
            .iter()
            .filter(|channel| {
                (channel.hidden && user_can_see_hidden_channels)
                    || (!channel.hidden && user_can_see_channel)
            })
            .collect::<Vec<_>>();
        (StatusCode::OK, serde_json::to_string(&channels).unwrap()).into_response()
    }
}
