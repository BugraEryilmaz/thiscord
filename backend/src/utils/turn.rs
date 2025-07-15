use axum::Router;
use axum::response::IntoResponse;
use axum::routing::get;
use axum_login::login_required;
use base64::{Engine as _, engine::general_purpose};
use chrono::{Duration, Utc};
use hmac::{Hmac, Mac};
use sha1::Sha1;

use crate::models::Backend;

pub fn generate_turn_credentials(username_ttl_secs: i64) -> (String, String, String) {
    let secret = std::env::var("TURN_SECRET").expect("TURN_SECRET must be set in the environment");
    let realm = std::env::var("TURN_REALM").expect("TURN_REALM must be set in the environment");
    let expiration = Utc::now() + Duration::seconds(username_ttl_secs);
    let username = expiration.timestamp().to_string();

    let mut mac = Hmac::<Sha1>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(username.as_bytes());
    let credential = general_purpose::STANDARD.encode(mac.finalize().into_bytes());

    (username, credential, realm.to_string())
}

pub fn router() -> Router {
    Router::new()
        .route("/get-creds", get(get::get_turn_credentials))
        .route_layer(login_required!(Backend))
}

mod get {

    use axum::{Json, http::StatusCode};
    use shared::models::TurnCreds;

    use super::*;

    pub async fn get_turn_credentials() -> impl IntoResponse {
        let username_ttl_secs = 600; // 10 minutes
        let (username, credential, realm) = generate_turn_credentials(username_ttl_secs);

        let response = TurnCreds {
            username,
            credential,
            realm,
            expiration: (Utc::now() + Duration::seconds(username_ttl_secs)).to_rfc3339(),
        };

        (StatusCode::OK, Json(response))
    }
}
