//! Native-only credential storage and system-browser loopback notification.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rand::{RngCore, rngs::OsRng};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use thiscord_shared::account::GOOGLE_COMPLETION_PARAM;
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
/// What the loopback received for the current attempt: the one-time completion
/// secret after a successful Google step, or `None` for a bare failure
/// notification. Held only in memory until the sign-in flow takes it; never
/// persisted or logged. Deliberately not `Debug`.
#[derive(serde::Serialize)]
pub struct Received {
    completion: Option<String>,
}
struct Attempt {
    cancel: oneshot::Sender<()>,
    received: Arc<Mutex<Option<Received>>>,
}
#[derive(Default)]
pub struct CallbackState(Mutex<Option<Attempt>>);

#[tauri::command]
pub fn cancel_google(state: tauri::State<'_, CallbackState>) -> Result<(), String> {
    if let Some(attempt) = state.0.lock().map_err(|_| "Callback unavailable")?.take() {
        let _ = attempt.cancel.send(());
    }
    Ok(())
}
/// Takes what the current attempt's loopback received, once; `None` while waiting.
#[tauri::command]
pub fn google_callback(state: tauri::State<'_, CallbackState>) -> Result<Option<Received>, String> {
    let state = state.0.lock().map_err(|_| "Callback unavailable")?;
    let Some(attempt) = state.as_ref() else {
        return Ok(None);
    };
    Ok(attempt
        .received
        .lock()
        .map_err(|_| "Callback unavailable")?
        .take())
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
    // Each attempt has its own slot, so a cancelled listener cannot fill a newer one.
    let received = Arc::new(Mutex::new(None));
    *state.0.lock().map_err(|_| "Callback unavailable")? = Some(Attempt {
        cancel,
        received: received.clone(),
    });
    tauri::async_runtime::spawn(async move {
        let hit = serve_callback(listener, path, cancelled, Duration::from_secs(300)).await;
        if let (Some(hit), Ok(mut slot)) = (hit, received.lock()) {
            *slot = Some(hit);
        }
    });
    Ok(callback)
}

fn base64url_secret(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
/// Accepts exactly `GET <path> HTTP/1.1` (a failure notification) or
/// `GET <path>?completion=<43 base64url characters> HTTP/1.1`; anything else,
/// including extra parameters, is rejected.
fn parse_request_line(line: &str, path: &str) -> Option<Received> {
    let target = line.strip_prefix("GET ")?.strip_suffix(" HTTP/1.1")?;
    let rest = target.strip_prefix(path)?;
    if rest.is_empty() {
        return Some(Received { completion: None });
    }
    let completion = rest
        .strip_prefix('?')?
        .strip_prefix(GOOGLE_COMPLETION_PARAM)?
        .strip_prefix('=')?;
    base64url_secret(completion).then(|| Received {
        completion: Some(completion.to_owned()),
    })
}

/// Serves one accepted hit on the loopback listener and returns what it carried.
async fn serve_callback(
    listener: tokio::net::TcpListener,
    path: String,
    mut cancelled: oneshot::Receiver<()>,
    timeout: Duration,
) -> Option<Received> {
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
            let Some(received) = request
                .lines()
                .next()
                .and_then(|line| parse_request_line(line, &path))
            else {
                let _ = socket
                    .write_all(
                        b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                    )
                    .await;
                continue;
            };
            // The page never repeats the secret and must not be cached.
            let body = "Sign-in finished. Return to Thiscord. You can close this tab.";
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nCache-Control: no-store\r\nReferrer-Policy: no-referrer\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = socket.write_all(response.as_bytes()).await;
            return Some(received);
        }
        None
    };
    tokio::select! {
        served = tokio::time::timeout(timeout, serve) => served.ok().flatten(),
        _ = &mut cancelled => None,
    }
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
    // Open exactly the URL that was validated, re-serialized by the parser.
    let url = String::from(url);
    tauri::async_runtime::spawn_blocking(move || {
        open::that(url).map_err(|_| "Could not open the system browser".into())
    })
    .await
    .map_err(|_| "Browser task failed".to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    fn completion() -> String {
        URL_SAFE_NO_PAD.encode([5u8; 32])
    }
    #[test]
    fn loopback_accepts_only_the_exact_path_and_one_strict_secret() {
        let path = format!("/thiscord/{}", URL_SAFE_NO_PAD.encode([1u8; 32]));
        let secret = completion();
        let line = |target: &str| format!("GET {target} HTTP/1.1");
        let accepted = parse_request_line(&line(&format!("{path}?completion={secret}")), &path)
            .and_then(|r| r.completion);
        assert_eq!(accepted.as_deref(), Some(secret.as_str()));
        // The bare path is the backend's failure notification, without a secret.
        assert!(parse_request_line(&line(&path), &path).is_some_and(|r| r.completion.is_none()));
        let short = &secret[1..];
        let long = format!("{secret}A");
        let bad_char = format!("{}+", &secret[1..]);
        let encoded = format!("{}%41", &secret[3..]);
        let other = URL_SAFE_NO_PAD.encode([2u8; 32]);
        for target in [
            format!("{path}?completion={secret}&x=1"),
            format!("{path}?x=1&completion={secret}"),
            format!("{path}?completion={secret}&completion={secret}"),
            format!("{path}?completion={short}"),
            format!("{path}?completion={long}"),
            format!("{path}?completion={bad_char}"),
            format!("{path}?completion={encoded}"),
            format!("{path}?completion="),
            format!("{path}?secret={secret}"),
            format!("{path}?"),
            format!("{path}/?completion={secret}"),
            format!("{path}x?completion={secret}"),
            format!("{path}#completion={secret}"),
            format!("/thiscord/{other}?completion={secret}"),
            format!("/other?completion={secret}"),
        ] {
            assert!(
                parse_request_line(&line(&target), &path).is_none(),
                "{target}"
            );
        }
        let valid = format!("{path}?completion={secret}");
        for request in [
            format!("POST {valid} HTTP/1.1"),
            format!("GET {valid} HTTP/1.0"),
            format!("GET  {valid} HTTP/1.1"),
            format!("GET {valid} HTTP/1.1 extra"),
            format!("get {valid} HTTP/1.1"),
        ] {
            assert!(parse_request_line(&request, &path).is_none(), "{request}");
        }
    }
    #[tokio::test(flavor = "current_thread")]
    async fn loopback_rejects_wrong_requests_and_returns_the_secret_once() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (_cancel, receiver) = oneshot::channel();
        let server = tokio::spawn(serve_callback(
            listener,
            "/thiscord/expected".into(),
            receiver,
            Duration::from_secs(5),
        ));
        let secret = completion();
        for (target, status) in [
            ("/thiscord/wrong".to_owned(), "404"),
            (format!("/thiscord/expected?completion={secret}&x=1"), "404"),
            (
                format!("/thiscord/expected?completion={}", &secret[1..]),
                "404",
            ),
            (format!("/thiscord/expected?completion={secret}"), "200"),
        ] {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            stream
                .write_all(format!("GET {target} HTTP/1.1\r\nHost: {address}\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut response = String::new();
            stream.read_to_string(&mut response).await.unwrap();
            assert!(response.starts_with(&format!("HTTP/1.1 {status}")));
            if status == "200" {
                assert!(response.contains("Cache-Control: no-store"));
                assert!(!response.contains(&secret));
            }
        }
        let received = server.await.unwrap().and_then(|r| r.completion);
        assert_eq!(received.as_deref(), Some(secret.as_str()));
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn loopback_bare_notification_and_oversized_requests() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (_cancel, receiver) = oneshot::channel();
        let server = tokio::spawn(serve_callback(
            listener,
            "/thiscord/expected".into(),
            receiver,
            Duration::from_secs(5),
        ));
        // A request line beyond the 4 KiB limit is never accepted.
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let padding = "a".repeat(5000);
        stream
            .write_all(format!("GET /thiscord/expected?{padding} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response).await;
        assert!(!response.starts_with("HTTP/1.1 200"));
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
            .write_all(b"GET /thiscord/expected HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        assert!(response.starts_with("HTTP/1.1 200"));
        assert!(
            server
                .await
                .unwrap()
                .is_some_and(|r| r.completion.is_none())
        );
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
            assert!(server.await.unwrap().is_none());
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
