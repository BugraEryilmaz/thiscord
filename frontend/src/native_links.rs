#[tauri::command]
pub async fn open_external_url(url: String) -> Result<(), String> {
    let url = thiscord_frontend::message_links::web_url(&url).ok_or("Invalid web URL")?;
    tauri::async_runtime::spawn_blocking(move || {
        open::that(url.as_str()).map_err(|_| "Could not open the system browser".into())
    })
    .await
    .map_err(|_| "Browser task failed".to_string())?
}
