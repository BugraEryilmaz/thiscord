use crate::models::{AuthSession, Backend};
use crate::utils::images::upload_image;
use axum::Router;
use axum::extract::Multipart;
use axum::response::IntoResponse;
use axum::{extract::DefaultBodyLimit, routing::post, routing::get};
use axum_login::login_required;
use futures_util::TryStreamExt;
use tokio_util::io::StreamReader;
use tower_http::limit::RequestBodyLimitLayer;

pub fn router() -> Router {
    Router::new()
        .route("/get-servers", get(get::get_servers))
        .route("/join-server", post(post::join_server))
        .route("/create-server", post(post::create_server))
        .route("/get-permissions/{server_id}", get(get::get_permissions))
        .layer(RequestBodyLimitLayer::new(5 * 1024 * 1024)) // 5MB limit
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024)) // 5MB limit
        .route_layer(login_required!(Backend))
}

mod post {
    use axum::Json;

    use super::*;

    pub async fn create_server(auth: AuthSession, mut multipart: Multipart) -> impl IntoResponse {
        let mut server_name = None;
        let mut server_image = None;
        while let Some(field) = multipart.next_field().await.unwrap() {
            let name = field.name().unwrap_or("unknown");
            if name == "server-name" {
                let value = match field.text().await {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::error!("Failed to read server name: {}", e);
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            "Invalid server name".to_string(),
                        );
                    }
                };
                server_name = Some(value);
            } else if name == "server-image" {
                let filename = match field.file_name() {
                    Some(f) => f.to_string(),
                    None => {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            "Server image has no filename".to_string(),
                        );
                    }
                };
                let content_type = match field.content_type() {
                    Some(ct) => ct.to_string(),
                    None => {
                        return (
                            axum::http::StatusCode::BAD_REQUEST,
                            "Server image has no content type".to_string(),
                        );
                    }
                };
                if !content_type.starts_with("image/") {
                    return (
                        axum::http::StatusCode::BAD_REQUEST,
                        "Invalid image type".to_string(),
                    );
                }
                let field = field.map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("Failed to read field: {}", e),
                    )
                });
                let stream = StreamReader::new(field);
                match upload_image(stream, filename).await {
                    Ok(filename) => {
                        tracing::info!("Image uploaded successfully");
                        server_image = Some(filename);
                    }
                    Err(e) => {
                        tracing::error!("Failed to upload image: {}", e);
                        return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
                    }
                };
            } else {
                tracing::warn!("Received unknown field: {}", name);
            }
        }
        if server_name.is_none() {
            // remove the image if it was uploaded
            if let Some(image) = server_image {
                let _ = tokio::fs::remove_file(image).await;
            }
            return (
                axum::http::StatusCode::BAD_REQUEST,
                "Server name is required".to_string(),
            );
        }

        let backend = auth.backend;
        match backend.create_server(
            server_name.unwrap().as_str(),
            server_image.clone(),
            auth.user.unwrap().id,
        ) {
            Ok(connection_string) => {
                tracing::info!("Server created successfully");
                (axum::http::StatusCode::OK, connection_string)
            }
            Err(e) => {
                tracing::error!("Failed to create server: {}", e);
                // remove the image if it was uploaded
                if let Some(image) = server_image {
                    let _ = tokio::fs::remove_file(image).await;
                }
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }

    #[derive(serde::Deserialize)]
    pub struct ConnectionString  {
        pub connection_string: String,
    }

    pub async fn join_server(auth: AuthSession, Json(connection_string): Json<ConnectionString>) -> impl IntoResponse {
        let user = auth.user.unwrap();
        let connection_string = connection_string.connection_string;
        tracing::info!(
            "User {} is attempting to join server with connection string: {}",
            &user.username,
            connection_string
        );
        let backend = auth.backend;
        let server_id = match backend.get_server_by_connection_string(connection_string.as_str()) {
            Ok(Some(id)) => id,
            Ok(None) => {
                return (
                    axum::http::StatusCode::BAD_REQUEST,
                    "Server not found".to_string(),
                );
            }
            Err(e) => {
                tracing::error!(
                    "Failed to find server with connection string: {}, error: {}",
                    connection_string,
                    e.to_string()
                );
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
        };
        match backend.join_user_to_server(user.id, server_id) {
            Ok(_) => {}
            Err(e) => {
                tracing::error!("Failed to join server: {}", e);
                return (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
        }
        (
            axum::http::StatusCode::OK,
            "Joined server successfully".to_string(),
        )
    }

}
mod get {
    use axum::extract::Path;
    use uuid::Uuid;

    use super::*;
    
    pub async fn get_servers(auth: AuthSession) -> impl IntoResponse {
        let backend = auth.backend;
        match backend.get_servers_for_user(auth.user.unwrap().id) {
            Ok(servers) => {
                tracing::info!("Retrieved {} servers for user", servers.len());
                (axum::http::StatusCode::OK, serde_json::to_string(&servers).unwrap())
            }
            Err(e) => {
                tracing::error!("Failed to retrieve servers: {}", e);
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }

    pub async fn get_permissions(auth: AuthSession, Path(server_id): Path<Uuid>) -> impl IntoResponse {
        let backend = auth.backend;
        let user = auth.user.unwrap();
        match backend.get_user_permissions(&user, server_id) {
            Ok(permissions) => {
                tracing::info!("Retrieved permissions for user {} on server {}", user.id, server_id);
                (axum::http::StatusCode::OK, serde_json::to_string(&permissions).unwrap())
            }
            Err(e) => {
                tracing::error!("Failed to retrieve permissions for user {} on server {}: {}", user.id, server_id, e);
                (axum::http::StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }
}