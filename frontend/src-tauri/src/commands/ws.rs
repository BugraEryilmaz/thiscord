use tauri::AppHandle;
use tokio::sync::mpsc::Receiver;

use crate::{commands::check_cookies, websocket::{websocket_handler, WebSocketRequest}};

pub fn connect_ws(handle: AppHandle, mut rx: Receiver<WebSocketRequest>) {
    tokio::spawn(async move {
        loop {
            // Check for cookies
            let handle = handle.clone();
            if !check_cookies(handle.clone()).await {
                tracing::warn!("No cookies found, cannot connect to WebSocket.");
                return;
            }

            let result = websocket_handler(handle.clone(), &mut rx).await;
            tracing::error!("WebSocket handler error: {:?}", result);
        }
    });
}