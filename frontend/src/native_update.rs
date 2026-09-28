//! Signed desktop updates. No account token is sent to the public release feed.
use std::{sync::Mutex, time::Duration};
use tauri::{Manager, State};
use tauri_plugin_updater::{Update, UpdaterExt};
use thiscord_shared::update::{UpdatePhase, UpdateStatus};

#[derive(Default)]
pub struct UpdateState {
    status: Mutex<UpdateStatus>,
    pending: Mutex<Option<Update>>,
    operation: tokio::sync::Mutex<()>,
    // Serialize voice admission with installation, including the async leave
    // performed when moving between channels. Checking for updates stays independent.
    pub voice_admission: tokio::sync::Mutex<()>,
}
impl UpdateState {
    fn change(&self, f: impl FnOnce(&mut UpdateStatus)) {
        if let Ok(mut status) = self.status.lock() {
            f(&mut status);
        }
    }
    fn failure(&self, message: &str) -> String {
        self.change(|s| {
            s.phase = UpdatePhase::Failed;
            s.message = message.into();
        });
        message.into()
    }
}

fn unsupported() -> Option<&'static str> {
    if cfg!(debug_assertions) {
        return Some("Install a release build to use automatic updates.");
    }
    #[cfg(target_os = "linux")]
    if std::env::var_os("APPIMAGE").is_none() {
        return Some(
            "Automatic updates require the AppImage. Update Debian packages with your package manager.",
        );
    }
    None
}

pub fn start(app: &tauri::AppHandle) {
    app.state::<UpdateState>().change(|s| {
        s.current_version = app.package_info().version.to_string();
        if let Some(message) = unsupported() {
            s.phase = UpdatePhase::Unsupported;
            s.message = message.into();
        }
    });
    if unsupported().is_some() {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(Duration::from_secs(3)).await;
        loop {
            let _ = check(&app).await;
            tokio::time::sleep(Duration::from_secs(6 * 60 * 60)).await;
        }
    });
}

#[tauri::command]
pub fn update_status(state: State<'_, UpdateState>) -> Result<UpdateStatus, String> {
    state
        .status
        .lock()
        .map(|s| s.clone())
        .map_err(|_| "Update status unavailable".into())
}

#[tauri::command]
pub async fn update_check(app: tauri::AppHandle) -> Result<(), String> {
    check(&app).await
}

async fn check(app: &tauri::AppHandle) -> Result<(), String> {
    if let Some(message) = unsupported() {
        return Err(message.into());
    }
    let state = app.state::<UpdateState>();
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    state.change(|s| {
        s.phase = UpdatePhase::Checking;
        s.message = "Checking for updates...".into();
    });
    let result = async {
        let updater = app
            .updater_builder()
            .timeout(Duration::from_secs(20))
            .build()
            .map_err(|_| ())?;
        updater.check().await.map_err(|_| ())
    }
    .await;
    match result {
        Ok(update) => {
            if let Some(update) = &update {
                if !valid_download(&update.download_url, &update.version) {
                    return Err(
                        state.failure("The update manifest contains an unsupported download URL.")
                    );
                }
                state.change(|s| {
                    s.phase = UpdatePhase::Available;
                    s.available_version = Some(update.version.clone());
                    s.message =
                        "An update is ready to download. Install when you are ready to restart."
                            .into();
                });
            } else {
                state.change(|s| {
                    s.phase = UpdatePhase::Idle;
                    s.available_version = None;
                    s.message = "Thiscord is up to date.".into();
                });
            }
            *state
                .pending
                .lock()
                .map_err(|_| "Update state unavailable")? = update;
            Ok(())
        }
        Err(()) => Err(state
            .failure("Could not check for updates. Check your connection and try again later.")),
    }
}

fn valid_download(url: &url::Url, version: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
        && url.query().is_none()
        && url.fragment().is_none()
        && url
            .path()
            .strip_prefix(&format!(
                "/BugraEryilmaz/thiscord/releases/download/client-v{version}/"
            ))
            .is_some_and(|file| !file.is_empty() && !file.contains('/') && !file.contains('%'))
}

pub fn installing(app: &tauri::AppHandle) -> bool {
    app.state::<UpdateState>()
        .status
        .lock()
        .map(|s| matches!(s.phase, UpdatePhase::Downloading | UpdatePhase::Installing))
        .unwrap_or(true)
}

#[tauri::command]
pub async fn update_install(app: tauri::AppHandle) -> Result<(), String> {
    if let Some(message) = unsupported() {
        return Err(message.into());
    }
    let state = app.state::<UpdateState>();
    let _voice_admission = state
        .voice_admission
        .try_lock()
        .map_err(|_| "A voice connection is changing. Try again after disconnecting.")?;
    let _operation = state
        .operation
        .try_lock()
        .map_err(|_| "An update operation is already running")?;
    let mut update = state
        .pending
        .lock()
        .map_err(|_| "Update state unavailable")?
        .clone()
        .ok_or("Check for an update first")?;
    if crate::native_voice::voice_status(app.state())?
        .channel_id
        .is_some()
    {
        return Err("Disconnect from voice before installing an update.".into());
    }
    state.change(|s| {
        s.phase = UpdatePhase::Downloading;
        s.downloaded = 0;
        s.total = None;
        s.message = "Downloading and verifying update...".into();
    });
    update.timeout = Some(Duration::from_secs(15 * 60));
    let too_large = tokio::sync::Notify::new();
    let result = tokio::select! { biased;
        _ = too_large.notified() => return Err(state.failure("The update exceeds the 512 MiB download limit.")),
        result = update.download(|chunk, total| {
            state.change(|s| {
                s.downloaded = s.downloaded.saturating_add(chunk as u64);
                s.total = total;
                if s.downloaded > 512 * 1024 * 1024 || total.is_some_and(|n| n > 512 * 1024 * 1024) { too_large.notify_one(); }
            });
        }, || {}) => result,
    };
    let bytes = result.map_err(|_| state.failure("Update download or signature verification failed. Nothing was installed; try again later."))?;
    if bytes.len() > 512 * 1024 * 1024 {
        return Err(state.failure("The update exceeds the 512 MiB download limit."));
    }
    // Recheck after the await: a voice join may have raced the initial check.
    if crate::native_voice::voice_status(app.state())?
        .channel_id
        .is_some()
    {
        return Err(state.failure("Disconnect from voice, then retry installing the update."));
    }
    state.change(|s| {
        s.phase = UpdatePhase::Installing;
        s.message = "Installing update and restarting...".into();
    });
    let engine = app
        .state::<crate::native_audio::AudioState>()
        .engine
        .clone();
    crate::native_audio::unregister_ptt(&app);
    let result = tauri::async_runtime::spawn_blocking(move || {
        // Release device handles before the Windows installer exits the process.
        let _ = engine.command(thiscord_frontend::audio::Command::Stop);
        update.install(bytes)
    })
    .await;
    match result {
        Ok(Ok(())) => app.restart(),
        _ => Err(state.failure(
            "The installer could not finish. Close other Thiscord windows and try again.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn downloads_are_bound_to_the_expected_https_release() {
        let good = "https://github.com/BugraEryilmaz/thiscord/releases/download/client-v0.2.0/Thiscord.exe";
        assert!(valid_download(&good.parse().unwrap(), "0.2.0"));
        assert!(!valid_download(&good.parse().unwrap(), "0.3.0"));
        for bad in [
            good.replace("https:", "http:"),
            good.replace("github.com/", "github.com.evil.test/"),
            good.replace("github.com/", "user@github.com/"),
            good.replace("Thiscord.exe", "nested/file.exe"),
            format!("{good}?token=x"),
        ] {
            assert!(!valid_download(&bad.parse().unwrap(), "0.2.0"));
        }
    }
}
