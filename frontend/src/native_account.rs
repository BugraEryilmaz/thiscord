//! Native-only credential storage and system-browser loopback notification.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use std::{sync::Mutex, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::oneshot,
};

const SERVICE: &str = "tr.com.thiscord.desktop";
fn account() -> &'static str {
    option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000")
}

/// OS credential storage through `keyring`, with a macOS-specific read.
mod store {
    fn entry(service: &str, account: &str) -> Result<keyring::Entry, ()> {
        keyring::Entry::new(service, account).map_err(|_| ())
    }
    #[cfg(not(target_os = "macos"))]
    pub fn load(service: &str, account: &str) -> Result<Option<String>, ()> {
        match entry(service, account)?.get_password() {
            Ok(token) => Ok(Some(token)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(_) => Err(()),
        }
    }
    /// Reads the login Keychain item `keyring` writes, then re-creates it.
    ///
    /// Keychain items trust the signing requirement of the build that created
    /// them. A build with another requirement (any earlier ad-hoc release) must
    /// ask before reading, and keeps asking on every launch unless the user
    /// chooses Always Allow. Deleting and adding items needs no approval, so
    /// after one approved read the item is re-added as this build's own. Later
    /// writes and reads then match without prompting.
    #[cfg(target_os = "macos")]
    pub fn load(service: &str, account: &str) -> Result<Option<String>, ()> {
        use security_framework::os::macos::{
            keychain::{SecKeychain, SecPreferencesDomain},
            passwords::find_generic_password,
        };
        const NOT_FOUND: i32 = -25300; // errSecItemNotFound
        let keychain =
            SecKeychain::default_for_domain(SecPreferencesDomain::User).map_err(|_| ())?;
        match find_generic_password(Some(std::slice::from_ref(&keychain)), service, account) {
            Ok((password, item)) => {
                let token = String::from_utf8(password.to_owned()).map_err(|_| ())?;
                item.delete();
                // On failure the caller still holds the token; the rotated
                // replacement is saved (added) again after sign-in.
                let _ = keychain.add_generic_password(service, account, token.as_bytes());
                Ok(Some(token))
            }
            Err(error) if error.code() == NOT_FOUND => Ok(None),
            Err(_) => Err(()),
        }
    }
    pub fn save(service: &str, account: &str, token: &str) -> Result<(), ()> {
        entry(service, account)?.set_password(token).map_err(|_| ())
    }
    pub fn clear(service: &str, account: &str) -> Result<(), ()> {
        match entry(service, account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(_) => Err(()),
        }
    }
}

#[tauri::command]
pub async fn load_session() -> Result<Option<String>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        store::load(SERVICE, account())
            .map_err(|()| "Unlock your OS credential store to restore your session".to_string())
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
        store::save(SERVICE, account(), &token)
            .map_err(|()| "Could not save session in OS credential storage".to_string())
    })
    .await
    .map_err(|_| "Credential storage task failed".to_string())?
}
#[tauri::command]
pub async fn clear_session(app: tauri::AppHandle) -> Result<(), String> {
    use tauri::Manager;
    let _ = crate::native_notifications::notification_clear(app.clone());
    // Clearing credentials also cancels a sleeping/in-flight voice retry.
    crate::native_voice::voice_leave(app.clone(), app.state()).await?;
    tauri::async_runtime::spawn_blocking(|| {
        store::clear(SERVICE, account())
            .map_err(|()| "Could not remove the stored session".to_string())
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
        let service = "tr.com.thiscord.test-only";
        let name = format!("test-{}", std::process::id());
        let first = URL_SAFE_NO_PAD.encode([42u8; 32]);
        let second = URL_SAFE_NO_PAD.encode([7u8; 32]);
        store::clear(service, &name).unwrap();
        assert_eq!(store::load(service, &name).unwrap(), None);
        store::save(service, &name, &first).unwrap();
        // Same order as a launch: read, then replace the rotated token.
        assert_eq!(
            store::load(service, &name).unwrap().as_deref(),
            Some(&*first)
        );
        store::save(service, &name, &second).unwrap();
        assert_eq!(
            store::load(service, &name).unwrap().as_deref(),
            Some(&*second)
        );
        store::clear(service, &name).unwrap();
        assert_eq!(store::load(service, &name).unwrap(), None);
        store::clear(service, &name).unwrap();
    }
}
