use gloo_net::http::Request;
use thiscord_shared::{ApiError, account::*};
use wasm_bindgen::prelude::*;

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
pub async fn request(
    command: &AccountRequest,
    token: Option<&str>,
) -> Result<AccountResponse, String> {
    let base = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
    let mut request = Request::post(&format!("{}{ACCOUNT_PATH}", base.trim_end_matches('/')));
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
        return Err(format!("{} (request {})", error.message, error.request_id));
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
