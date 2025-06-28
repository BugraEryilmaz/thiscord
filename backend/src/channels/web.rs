use axum::http::StatusCode;
use uuid::Uuid;

use crate::Error;
use shared::models::NewChannel;
use shared::models::PermissionType;
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
    use futures_util::future::join_all;

    use crate::{models::user::OnlineUsers, utils::SubscribableOnce};

    use super::*;

    pub async fn list_channels(
        session: AuthSession,
        Path(server_id): Path<Uuid>,
    ) -> impl IntoResponse {
        let user = session.user.unwrap();
        let internal_err =
            |e: Error| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        let server = match session.backend.get_server(server_id) {
            Ok(server) => server,
            Err(e) => {
                return internal_err(e);
            }
        };
        let online_user = OnlineUsers::get().get_user(user.0.id);
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
            .into_iter()
            .filter(|channel| {
                (channel.hidden && user_can_see_hidden_channels)
                    || (!channel.hidden && user_can_see_channel)
            });
        // Convert the channels to channels with users
        let channels = channels.into_iter().map(async |channel| {
            Backend::convert_channel_to_with_users(channel).await
        }).collect::<Vec<_>>();
        let channels = join_all(channels).await;
        if let Some(online_user) = online_user {
            server.subscribe(&online_user);
        }
        (StatusCode::OK, serde_json::to_string(&channels).unwrap()).into_response()
    }
}
