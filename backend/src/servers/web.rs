use crate::models::{AuthSession, Backend};
use crate::utils::images::upload_image;
use axum::Router;
use axum::extract::Multipart;
use axum::response::IntoResponse;
use axum::{extract::DefaultBodyLimit, routing::post};
use axum_login::login_required;
use futures_util::TryStreamExt;
use tokio_util::io::StreamReader;
use tower_http::limit::RequestBodyLimitLayer;

pub fn router() -> Router {
    Router::new()
        .route("/create-server", post(post::create_server))
        .layer(RequestBodyLimitLayer::new(5 * 1024 * 1024)) // 5MB limit
        .layer(DefaultBodyLimit::max(5 * 1024 * 1024)) // 5MB limit
        .route_layer(login_required!(Backend))
}

mod post {
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
        match backend
            .create_server(server_name.unwrap().as_str(), server_image.clone(), auth.user.unwrap().id)
            .await
        {
            Ok(_) => {
                tracing::info!("Server created successfully");
                (axum::http::StatusCode::OK, "Server created".to_string())
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
}
