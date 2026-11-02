use gloo_net::http::Request;
use thiscord_shared::{ApiError, account::*};
use wasm_bindgen::prelude::*;

pub fn admin_page() -> bool {
    !desktop()
        && web_sys::window()
            .and_then(|w| w.location().pathname().ok())
            .is_some_and(|p| p == "/admin" || p.starts_with("/admin/"))
}
/// The browser web client (served at `/app`, or by `trunk serve` with its API
/// proxy). It is same-origin with the API and authenticates with an HttpOnly
/// session cookie, so it never holds a readable session token.
pub fn web_app() -> bool {
    !desktop() && !admin_page()
}
/// Empty for same-origin browser pages (web client and admin dashboard); the
/// desktop uses the compile-time `THISCORD_API_URL`.
pub fn api_base() -> &'static str {
    if desktop() {
        option_env!("THISCORD_API_URL")
            .unwrap_or("http://localhost:3000")
            .trim_end_matches('/')
    } else {
        ""
    }
}
/// Absolute HTTP(S) origin of the API, for links shown to users.
pub fn public_base() -> String {
    if api_base().is_empty() {
        web_sys::window()
            .and_then(|w| w.location().origin().ok())
            .unwrap_or_default()
    } else {
        api_base().to_owned()
    }
}
/// WS(S) origin of the API for socket URLs.
pub fn socket_base() -> String {
    public_base().replacen("http", "ws", 1)
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["__TAURI__", "core"], js_name = invoke)]
    async fn native_invoke(command: &str, args: JsValue) -> Result<JsValue, JsValue>;
}
pub fn desktop() -> bool {
    js_sys::Reflect::has(&js_sys::global(), &JsValue::from_str("__TAURI__")).unwrap_or(false)
}
pub async fn native<T: serde::de::DeserializeOwned>(
    command: &str,
    args: serde_json::Value,
) -> Result<T, String> {
    let value = native_invoke(
        command,
        serde_wasm_bindgen::to_value(&args).map_err(|_| "Invalid command")?,
    )
    .await
    .map_err(|e| {
        e.as_string()
            .unwrap_or_else(|| "Desktop integration failed".into())
    })?;
    serde_wasm_bindgen::from_value(value).map_err(|_| "Invalid desktop response".into())
}

#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(catch, js_namespace = ["__TAURI__", "event"], js_name = listen)]
    async fn event_listen(name: &str, callback: &js_sys::Function) -> Result<JsValue, JsValue>;
}
pub struct EventListener {
    unlisten: js_sys::Function,
    _callback: Closure<dyn FnMut(JsValue)>,
}
impl Drop for EventListener {
    fn drop(&mut self) {
        let _ = self.unlisten.call0(&JsValue::NULL);
    }
}
pub async fn listen<T: serde::de::DeserializeOwned + 'static>(
    name: &str,
    mut callback: impl FnMut(T) + 'static,
) -> Result<EventListener, String> {
    let callback = Closure::wrap(Box::new(move |event: JsValue| {
        if let Ok(payload) = js_sys::Reflect::get(&event, &JsValue::from_str("payload"))
            && let Ok(value) = serde_wasm_bindgen::from_value(payload)
        {
            callback(value);
        }
    }) as Box<dyn FnMut(JsValue)>);
    let unlisten = event_listen(name, callback.as_ref().unchecked_ref())
        .await
        .map_err(|_| "Desktop events unavailable")?
        .dyn_into()
        .map_err(|_| "Invalid desktop listener")?;
    Ok(EventListener {
        unlisten,
        _callback: callback,
    })
}
pub async fn request(
    command: &AccountRequest,
    token: Option<&str>,
) -> Result<AccountResponse, String> {
    api_request(ACCOUNT_PATH, command, token).await
}
pub async fn api_request<T: serde::Serialize, R: serde::de::DeserializeOwned>(
    path: &str,
    command: &T,
    token: Option<&str>,
) -> Result<R, String> {
    let base = api_base();
    let mut request = Request::post(&format!("{}{path}", base.trim_end_matches('/')));
    if let Some(token) = token {
        request = request.header("Authorization", &format!("Bearer {token}"));
    }
    let response = request
        .json(command)
        .map_err(|_| "Invalid request")?
        .send()
        .await
        .map_err(|_| "Cannot reach Thiscord. Check your connection")?;
    if !response.ok() {
        let error = response
            .json::<ApiError>()
            .await
            .map_err(|_| "Server returned an invalid response")?;
        return Err(crate::i18n::api_error(&error));
    }
    response
        .json()
        .await
        .map_err(|_| "Server returned an invalid response".into())
}
pub async fn persist(token: Option<&str>) -> Result<(), String> {
    if desktop() {
        if let Some(token) = token {
            native("save_session", serde_json::json!({"token":token})).await
        } else {
            native("clear_session", serde_json::json!({})).await
        }
    } else {
        Ok(())
    }
}

/// Confirm a socket authentication failure with HTTP. Older servers can report
/// socket rate limits/service failures as Unauthorized; those remain retryable.
pub async fn session_rejected(token: &str) -> bool {
    let check = async {
        let Ok(request) = Request::post(&format!("{}{ACCOUNT_PATH}", api_base()))
            .header("Authorization", &format!("Bearer {token}"))
            .json(&AccountRequest::Current)
        else {
            return false;
        };
        request
            .send()
            .await
            .is_ok_and(|response| response.status() == 401)
    };
    matches!(
        futures_util::future::select(
            Box::pin(check),
            Box::pin(gloo_timers::future::TimeoutFuture::new(10_000)),
        )
        .await,
        futures_util::future::Either::Left((true, _))
    )
}
