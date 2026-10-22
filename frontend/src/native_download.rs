//! WebView-managed Blob downloads; no arbitrary filesystem-write IPC capability.
use tauri::{Emitter, Manager, webview::DownloadEvent};
use thiscord_shared::chat::{CHAT_DOWNLOAD_EVENT, ChatDownload};

pub fn create_main_window(app: &tauri::App) -> tauri::Result<()> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == "main")
        .expect("main window configuration");
    tauri::WebviewWindowBuilder::from_config(app, config)?
        .on_download(|webview, event| {
            match event {
                DownloadEvent::Requested { url, destination } => {
                    // Attachments and diagnostics use Blob URLs after their own
                    // authenticated fetch. Never turn navigation into a download.
                    if url.scheme() != "blob" {
                        return false;
                    }
                    let directory = webview
                        .app_handle()
                        .path()
                        .download_dir()
                        .ok()
                        .filter(|path| std::fs::create_dir_all(path).is_ok());
                    let Some(directory) = directory else {
                        let _ = webview.emit(
                            CHAT_DOWNLOAD_EVENT,
                            ChatDownload {
                                url: url.to_string(),
                                success: false,
                            },
                        );
                        return false;
                    };
                    let name = destination
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("attachment");
                    *destination = directory.join(download_name(name));
                }
                DownloadEvent::Finished { url, success, .. } => {
                    // macOS does not provide a final path, even on success.
                    let _ = webview.emit(
                        CHAT_DOWNLOAD_EVENT,
                        ChatDownload {
                            url: url.to_string(),
                            success,
                        },
                    );
                }
                _ => {}
            }
            true
        })
        .build()?;
    Ok(())
}

fn download_name(suggested: &str) -> String {
    // A fixed prefix avoids Windows device names; a random suffix prevents
    // repeated downloads from silently overwriting an existing file.
    let mut safe: String = suggested
        .chars()
        .take(180)
        .map(|c| {
            if c.is_control() || "<>:\"/\\|?*".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    while safe.len() > 180 {
        safe.pop();
    }
    let safe = safe.trim_matches([' ', '.']);
    format!(
        "thiscord-{:016x}-{}",
        rand::random::<u64>(),
        if safe.is_empty() { "attachment" } else { safe }
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_have_safe_unique_basenames() {
        for name in ["CON", "../secret.txt", "a\\b:c?.txt", "", "..", "x\n.txt"] {
            let result = download_name(name);
            assert!(result.starts_with("thiscord-"));
            assert!(
                !result
                    .chars()
                    .any(|c| c.is_control() || "<>:\"/\\|?*".contains(c))
            );
            assert_eq!(std::path::Path::new(&result).components().count(), 1);
            assert_ne!(result, download_name(name));
        }
        assert!(download_name("photo.png").ends_with("photo.png"));
        assert!(download_name(&"🖼".repeat(255)).len() < 255);
    }
}
