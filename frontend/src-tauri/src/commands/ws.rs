use front_shared::LoginStatus;
use tauri::{http, AppHandle};
use tokio::{
    runtime::Builder, sync::mpsc::Receiver, task::LocalSet
};

use crate::{
    commands::{check_cookies, logout},
    websocket::{websocket_handler, WebSocketRequest},
    Error,
};

pub fn connect_ws(handle: AppHandle, mut rx: Receiver<WebSocketRequest>) {
    let rt = Builder::new_current_thread().enable_all().build().unwrap();

    std::thread::spawn(move || {
        let local = LocalSet::new();

        local.spawn_local(async move {
            loop {
                // Check for cookies
                let handle = handle.clone();
                if !matches!(
                    check_cookies(handle.clone()).await,
                    Ok(LoginStatus::LoggedIn(_))
                ) {
                    tracing::warn!("No cookies found, cannot connect to WebSocket.");
                    return;
                }

                let result = websocket_handler(handle.clone(), &mut rx).await;
                match &result {
                    Err(Error::TokioTungsteniteError(
                        tokio_tungstenite::tungstenite::Error::Http(e),
                    )) => {
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
        rt.block_on(local);
    });
}
