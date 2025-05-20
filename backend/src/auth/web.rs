use axum::Router;
use axum::routing::post;

use crate::models::{AuthSession, Credentials};

pub fn router() -> Router {
    Router::new()
        .route("/login", post(self::post::login))
        .route("/signup", post(self::post::signup))
}

mod post {
    use crate::models::Signup;
    use axum::Json;
    use axum::http::StatusCode;
    use axum::response::IntoResponse;

    use super::*;

    pub async fn login(
        mut auth: AuthSession,
        Json(credentials): Json<Credentials>,
    ) -> impl IntoResponse {
        let user = match auth.authenticate(credentials).await {
            Ok(Some(user)) => user,
            Ok(None) => {
                tracing::info!("Failed to login user: Invalid credentials");
                return (StatusCode::UNAUTHORIZED, "Invalid credentials".to_string());
            }
            Err(e) => {
                tracing::error!("Failed to login user: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
        };
        match auth.login(&user).await {
            Ok(_) => {
                tracing::info!("User {} logged in", user.username);
                (StatusCode::OK, "Logged in".to_string())
            }
            Err(e) => {
                tracing::error!("Failed to login user {}: {}", user.username, e);
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }

    pub async fn signup(auth: AuthSession, Json(signup): Json<Signup>) -> impl IntoResponse {
        match auth.backend.check_username_exists(&signup.username) {
            Ok(true) => {
                tracing::info!("Failed to signup user: Username already exists");
                return (
                    StatusCode::BAD_REQUEST,
                    "Username already exists".to_string(),
                );
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!("Failed to check username existence: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
        }
        match auth.backend.check_email_exists(&signup.email) {
            Ok(true) => {
                tracing::info!("Failed to signup user: Email already exists");
                return (StatusCode::BAD_REQUEST, "Email already exists".to_string());
            }
            Ok(false) => {}
            Err(e) => {
                tracing::error!("Failed to check email existence: {}", e);
                return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string());
            }
        }
        match auth.backend.create_user(signup) {
            Ok(user) => {
                tracing::info!("User {} signed up", user.username);
                (StatusCode::OK, "Signed up".to_string())
            }
            Err(e) => {
                tracing::error!("Failed to signup user: {}", e);
                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            }
        }
    }
}
