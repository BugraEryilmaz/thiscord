//! Best-effort local diagnostics; session owners are not proof of exclusive ownership.
use super::AudioSettings;

pub(super) fn describe(settings: &AudioSettings, microphone: bool) -> String {
    #[cfg(target_os = "windows")]
    if let Ok(users) = windows_users(settings, microphone)
        && !users.is_empty()
    {
        return format!(
            "Active audio-session processes on selected/default devices (possible conflicts, not confirmed blockers): {}.",
            users.join("; ")
        );
    }
    let _ = (settings, microphone);
    "The OS could not identify a blocking process. Close other audio apps or check device exclusive-mode settings.".into()
}

#[cfg(target_os = "windows")]
fn windows_users(settings: &AudioSettings, microphone: bool) -> windows::core::Result<Vec<String>> {
    use windows::{
        Win32::{
            Foundation::CloseHandle,
            Media::Audio::{
                AudioSessionStateActive, IAudioSessionControl2, IAudioSessionManager2,
                IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eConsole, eRender,
            },
            System::{
                Com::{
                    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
                    CoUninitialize,
                },
                Threading::{
                    OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
                    QueryFullProcessImageNameW,
                },
            },
        },
        core::{HSTRING, Interface, PWSTR},
    };
    // Runs on a dedicated MTA worker. Every interface is released before COM cleanup.
    unsafe {
        CoInitializeEx(None, COINIT_MULTITHREADED).ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe {
                    CoUninitialize();
                }
            }
        }
        let _com = Com;
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
        let mut users = std::collections::BTreeSet::new();
        for (input, selected) in [
            (false, settings.output.as_deref()),
            (true, settings.input.as_deref()),
        ] {
            if input && !microphone {
                continue;
            }
            for id in [selected, None] {
                let endpoint = if let Some(id) = id {
                    let Ok(id) = id.parse::<cpal::DeviceId>() else {
                        continue;
                    };
                    enumerator.GetDevice(&HSTRING::from(id.id()))
                } else {
                    enumerator
                        .GetDefaultAudioEndpoint(if input { eCapture } else { eRender }, eConsole)
                };
                let Ok(endpoint) = endpoint else {
                    continue;
                };
                let Ok(manager) = endpoint.Activate::<IAudioSessionManager2>(CLSCTX_ALL, None)
                else {
                    continue;
                };
                let Ok(sessions) = manager.GetSessionEnumerator() else {
                    continue;
                };
                for index in 0..sessions.GetCount().unwrap_or(0).min(128) {
                    let Ok(session) = sessions.GetSession(index) else {
                        continue;
                    };
                    if session.GetState().ok() != Some(AudioSessionStateActive) {
                        continue;
                    }
                    let Ok(session) = session.cast::<IAudioSessionControl2>() else {
                        continue;
                    };
                    let Ok(pid) = session.GetProcessId() else {
                        continue;
                    };
                    if pid == 0 || pid == std::process::id() {
                        continue;
                    }
                    let mut name = String::new();
                    if let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) {
                        let mut buffer = [0u16; 1024];
                        let mut size = buffer.len() as u32;
                        if QueryFullProcessImageNameW(
                            handle,
                            PROCESS_NAME_WIN32,
                            PWSTR(buffer.as_mut_ptr()),
                            &mut size,
                        )
                        .is_ok()
                        {
                            let path = String::from_utf16_lossy(&buffer[..size as usize]);
                            name = path
                                .rsplit('\\')
                                .next()
                                .unwrap_or("")
                                .chars()
                                .filter(|c| !c.is_control())
                                .take(100)
                                .collect();
                        }
                        let _ = CloseHandle(handle);
                    }
                    users.insert(format!(
                        "{} {} (PID {pid})",
                        if input { "Microphone:" } else { "Output:" },
                        if name.is_empty() {
                            "unknown executable"
                        } else {
                            &name
                        }
                    ));
                    if users.len() >= 12 {
                        return Ok(users.into_iter().collect());
                    }
                }
            }
        }
        Ok(users.into_iter().collect())
    }
}
