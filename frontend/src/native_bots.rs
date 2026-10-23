//! Store newly issued bot credentials in the same namespace used by the SDK.
#[tauri::command]
pub async fn save_bot_credential(name: String, token: String) -> Result<(), String> {
    if token.len() != 47
        || !token.starts_with("bot_")
        || !token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("Invalid bot credential".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let server = option_env!("THISCORD_API_URL").unwrap_or("http://localhost:3000");
        thiscord_frontend::bot_credentials::entry(server, &name)?
            .set_password(&token)
            .map_err(|_| "Cannot save bot token in OS credential store".to_string())
    })
    .await
    .map_err(|_| "Credential storage task failed".to_string())?
}
