//! One UI command adapter for local IPC and the restricted admin relay.
use leptos::prelude::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::sync::Arc;
use thiscord_shared::{ApiError, RequestId, audio::AudioSettings, remote_audio};

#[derive(Clone, Copy, Default)]
pub struct AudioControl {
    remote: Option<Remote>,
}
#[derive(Clone, Copy)]
struct Remote {
    connection: RequestId,
    token: RwSignal<Option<String>>,
    expected: RwSignal<Option<AudioSettings>>,
    serial: StoredValue<Arc<futures_util::lock::Mutex<()>>>,
}
impl AudioControl {
    pub fn remote(connection: RequestId, token: RwSignal<Option<String>>) -> Self {
        Self {
            remote: Some(Remote {
                connection,
                token,
                expected: RwSignal::new(None),
                serial: StoredValue::new(Arc::new(futures_util::lock::Mutex::new(()))),
            }),
        }
    }
    pub fn is_remote(self) -> bool {
        self.remote.is_some()
    }
    pub fn observe(self, settings: &AudioSettings) {
        if let Some(remote) = self.remote {
            remote.expected.set(Some(settings.clone()));
        }
    }
    pub fn poll<T: DeserializeOwned + 'static>(
        self,
        name: &'static str,
        accept: impl Fn(Result<T, String>) + 'static,
    ) {
        let (abort, registration) = futures_util::future::AbortHandle::new_pair();
        on_cleanup(move || abort.abort());
        leptos::task::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(
                async move {
                    loop {
                        accept(self.invoke(name, json!({})).await);
                        gloo_timers::future::TimeoutFuture::new(3000).await;
                    }
                },
                registration,
            )
            .await;
        });
    }
    pub async fn invoke<T: DeserializeOwned>(
        self,
        name: &str,
        mut args: Value,
    ) -> Result<T, String> {
        let Some(remote) = self.remote else {
            return crate::account_client::native(name, args).await;
        };
        let serial = remote.serial.try_get_value().ok_or("Audio panel closed")?;
        let _guard = serial.lock().await;
        let token = remote
            .token
            .try_get_untracked()
            .flatten()
            .ok_or("Sign in again")?;
        if remote.expected.is_disposed() {
            return Err("Audio panel closed".into());
        }
        args["command"] = name.into();
        if name == "audio_save" {
            args["expected"] = serde_json::to_value(
                remote
                    .expected
                    .get_untracked()
                    .ok_or("Refresh audio settings first")?,
            )
            .map_err(|_| "Invalid settings")?;
        }
        let command: remote_audio::Command = serde_json::from_value(args)
            .map_err(|_| "This action is available only on the user's computer")?;
        command.validate().map_err(str::to_owned)?;
        let fetch = async {
            let response = gloo_net::http::Request::post(&format!(
                "{}{}",
                crate::account_client::api_base(),
                remote_audio::PATH
            ))
            .header("Authorization", &format!("Bearer {token}"))
            .json(&remote_audio::Request {
                connection_id: remote.connection,
                command,
            })
            .map_err(|_| "Invalid audio request")?
            .send()
            .await
            .map_err(|_| "Cannot reach the desktop. Check that the user is still in voice.")?;
            if !response.ok() {
                let status = response.status();
                let detail = response.json::<ApiError>().await.map(|e| {
                    let message = match status {
                        401 => "Your session expired. Sign in again.",
                        403 => "Remote audio requires current instance owner or admin access.",
                        409 => "This desktop connection ended. Close this dialog and select the user's current connection.",
                        503 => "The desktop did not respond. A change may have applied; refresh before retrying.",
                        _ => &e.message,
                    };
                    format!("{message} (request {})", e.request_id)
                }).unwrap_or_else(|_| "Remote audio is unavailable".into());
                return Err(detail);
            }
            response
                .json::<remote_audio::Reply>()
                .await
                .map_err(|_| "Invalid desktop response".to_owned())?
        };
        let value = match futures_util::future::select(
            Box::pin(fetch),
            Box::pin(gloo_timers::future::TimeoutFuture::new(65000)),
        )
        .await
        {
            futures_util::future::Either::Left((result, _)) => result?,
            futures_util::future::Either::Right(_) => {
                return Err("Desktop did not respond. Refresh before retrying a change.".into());
            }
        };
        if remote.expected.is_disposed()
            || remote.token.try_get_untracked().flatten().as_deref() != Some(token.as_str())
        {
            return Err("Audio panel closed or session changed".into());
        }
        serde_json::from_value(value).map_err(|_| "Invalid desktop audio response".into())
    }
    pub async fn download(self) -> Result<(), String> {
        use wasm_bindgen::JsCast;
        let value: Value = self.invoke("audio_diagnostics_download", json!({})).await?;
        let text =
            serde_json::to_string_pretty(&value).map_err(|_| "Cannot prepare diagnostics")?;
        let parts = js_sys::Array::new();
        parts.push(&text.into());
        let options = web_sys::BlobPropertyBag::new();
        options.set_type("application/json");
        let blob = web_sys::Blob::new_with_str_sequence_and_options(&parts, &options)
            .map_err(|_| "Cannot prepare download")?;
        let url = web_sys::Url::create_object_url_with_blob(&blob)
            .map_err(|_| "Cannot prepare download")?;
        let result = (|| {
            let document = web_sys::window()
                .and_then(|w| w.document())
                .ok_or("Browser unavailable")?;
            let link = document
                .create_element("a")
                .map_err(|_| "Cannot create download")?
                .dyn_into::<web_sys::HtmlAnchorElement>()
                .map_err(|_| "Cannot create download")?;
            link.set_href(&url);
            link.set_download(&format!(
                "thiscord-audio-{}.json",
                self.remote.ok_or("Remote desktop required")?.connection
            ));
            let body = document.body().ok_or("Browser unavailable")?;
            body.append_child(&link)
                .map_err(|_| "Cannot create download")?;
            link.click();
            link.remove();
            Ok::<_, String>(())
        })();
        gloo_timers::future::TimeoutFuture::new(1000).await;
        let _ = web_sys::Url::revoke_object_url(&url);
        result
    }
}
