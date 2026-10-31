//! Scheduling class for the audio worker. It runs DSP, codecs and playback
//! refills against device deadlines, so ordinary priority lets a busy game or
//! WebView delay it into capture overflows and playback starvation.
//!
//! This is deliberately not a realtime class: the worker also opens devices and
//! prepares models. Windows MMCSS "Audio" and macOS user-interactive QoS both
//! leave the system responsive if the thread overruns. Linux needs rtkit over
//! D-Bus for an unprivileged boost, which is not a dependency, so the worker
//! keeps its default policy there.

/// Applies once to the calling thread for its lifetime.
pub(super) fn promote_worker() -> Result<&'static str, String> {
    #[cfg(target_os = "windows")]
    {
        let mut task = 0u32;
        // SAFETY: a static NUL-terminated task name and a valid output pointer.
        // The returned handle is intentionally kept for the thread's lifetime.
        unsafe {
            windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(
                windows::core::w!("Audio"),
                &mut task,
            )
        }
        .map(|_| "mmcss_audio")
        .map_err(|error| error.to_string())
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
        // SAFETY: only changes the calling thread's QoS class.
        match unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) } {
            0 => Ok("qos_user_interactive"),
            code => Err(format!("pthread_set_qos_class_self_np failed: {code}")),
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        Ok("default")
    }
}
