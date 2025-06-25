use front_shared::LoginStatus;
use tauri::{http, AppHandle};
use tokio::sync::mpsc::Receiver;

use crate::{commands::{check_cookies, logout}, utils::handle_auth_error, websocket::{websocket_handler, WebSocketRequest}, Error};

pub fn connect_ws(handle: AppHandle, mut rx: Receiver<WebSocketRequest>) {
    tokio::spawn(async move {
        loop {
            // Check for cookies
            let handle = handle.clone();
            if !matches!(check_cookies(handle.clone()).await, Ok(LoginStatus::LoggedIn(_))) {
                tracing::warn!("No cookies found, cannot connect to WebSocket.");
                return;
            }

            let result = websocket_handler(handle.clone(), &mut rx).await;
            match &result {
                Err(Error::TokioTungsteniteError(tokio_tungstenite::tungstenite::Error::Http(e))) => {
                    if e.status() == http::StatusCode::UNAUTHORIZED {
                        let _ = logout(handle.clone()).await;
                        tracing::warn!("Unauthorized access detected, logging out.");
                    }
                }
                _ => {}
            }
            tracing::error!("WebSocket handler error: {:?}", result);
        }
    });
}