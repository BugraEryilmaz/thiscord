//! Native-only credential storage and system-browser loopback notification.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use std::{sync::Mutex, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};

fn entry() -> Result<keyring::Entry, String> {
    keyring::Entry::new(
        "tr.com.thiscord.desktop",
        option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000"),
    )
    .map_err(|_| "OS credential storage is unavailable".into())
}
#[tauri::command]
pub async fn load_session() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| match entry()?.get_password() {
        Ok(token) => Ok(Some(token)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(_) => Err("Unlock your OS credential store to restore your session".into()),
    })
    .await
    .map_err(|_| "Credential storage task failed".to_string())?
}
#[tauri::command]
pub async fn save_session(token: String) -> Result<(), String> {
    if token.len() != 43
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid session".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        entry()?
            .set_password(&token)
            .map_err(|_| "Could not save session in OS credential storage".into())
    })
    .await
    .map_err(|_| "Credential storage task failed".to_string())?
}
#[tauri::command]
pub async fn clear_session() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(|| match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(_) => Err("Could not remove the stored session".into()),
    })
    .await
    .map_err(|_| "Credential storage task failed".to_string())?
}
#[derive(Default)]
pub struct CallbackState(Mutex<Option<oneshot::Sender<()>>>);

#[tauri::command]
pub fn cancel_google(state: tauri::State<'_, CallbackState>) -> Result<(), String> {
    if let Some(sender) = state.0.lock().map_err(|_| "Callback unavailable")?.take() {
        let _ = sender.send(());
    }
    Ok(())
}
#[tauri::command]
pub async fn prepare_google(state: tauri::State<'_, CallbackState>) -> Result<String, String> {
    cancel_google(state.clone())?;
    // Allocate before opening the browser; another process cannot claim the port first.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .map_err(|_| "Cannot open desktop callback")?;
    let port = listener
        .local_addr()
        .map_err(|_| "Cannot inspect callback")?
        .port();
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    let path = format!("/thiscord/{}", URL_SAFE_NO_PAD.encode(bytes));
    let callback = format!("http://127.0.0.1:{port}{path}");
    let (cancel, cancelled) = oneshot::channel();
    *state.0.lock().map_err(|_| "Callback unavailable")? = Some(cancel);
    tauri::async_runtime::spawn(serve_callback(
        listener,
        path,
        cancelled,
        Duration::from_secs(300),
    ));
    Ok(callback)
}

async fn serve_callback(
    listener: tokio::net::TcpListener,
    path: String,
    mut cancelled: oneshot::Receiver<()>,
    timeout: Duration,
) {
    let serve = async {
        loop {
            let Ok((mut socket, _)) = listener.accept().await else {
                break;
            };
            let read = async {
                let mut buffer = Vec::new();
                let mut chunk = [0u8; 512];
                while buffer.len() < 4096 && !buffer.ends_with(b"\r\n\r\n") {
                    let n = socket.read(&mut chunk).await?;
                    if n == 0 {
                        break;
                    }
                    buffer.extend_from_slice(&chunk[..n]);
                }
                Ok::<_, std::io::Error>(buffer)
            };
            let Ok(Ok(buffer)) = tokio::time::timeout(Duration::from_secs(2), read).await else {
                continue;
            };
            let request = String::from_utf8_lossy(&buffer);
            if request.lines().next() != Some(&format!("GET {path} HTTP/1.1")) {
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                continue;
            }
            let body = "Sign-in finished. Return to Thiscord. You can close this tab.";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            break;
        }
    };
    tokio::select! { _ = tokio::time::timeout(timeout,serve) => {}, _ = &mut cancelled => {} }
}
#[tauri::command]
pub async fn open_google(authorization_url: String) -> Result<(), String> {
    let url = url::Url::parse(&authorization_url).map_err(|_| "Invalid authorization URL")?;
    if url.scheme() != "https"
        || url.host_str() != Some("accounts.google.com")
        || url.port().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || !matches!(url.path(), "/o/oauth2/v2/auth" | "/o/oauth2/auth")
    {
        return Err("Invalid Google authorization URL".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        open::that(authorization_url).map_err(|_| "Could not open the system browser".into())
    })
    .await
    .map_err(|_| "Browser task failed".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test(flavor = "current_thread")]
    async fn loopback_rejects_wrong_paths_and_closes_after_success() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (_cancel, receiver) = oneshot::channel();
        let server = tokio::spawn(serve_callback(
            listener,
            "/thiscord/expected".into(),
            receiver,
            Duration::from_secs(5),
        ));
        for (path, status) in [("/thiscord/wrong", "404"), ("/thiscord/expected", "200")] {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream
                .write_all(format!("GET {path} HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {status}")));
        }
        server.await.unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn loopback_closes_on_cancellation_and_timeout() {
        for cancel in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (sender, receiver) = oneshot::channel();
            let server = tokio::spawn(serve_callback(
                listener,
                "/thiscord/expected".into(),
                receiver,
                Duration::from_millis(20),
            ));
            if cancel {
                sender.send(()).unwrap();
            }
            server.await.unwrap();
            assert!(tokio::net::TcpStream::connect(address).await.is_err());
        }
    }
    #[test]
    #[ignore = "requires an unlocked OS credential store"]
    fn credential_store_round_trip() {
        let name = format!("test-{}", std::process::id());
        let entry = keyring::Entry::new("tr.com.thiscord.test-only", &name).unwrap();
        let token = URL_SAFE_NO_PAD.encode([42u8; 32]);
        entry.set_password(&token).unwrap();
        assert_eq!(entry.get_password().unwrap(), token);
        entry.delete_credential().unwrap();
        assert!(matches!(entry.get_password(), Err(keyring::Error::NoEntry)));
    }
}
