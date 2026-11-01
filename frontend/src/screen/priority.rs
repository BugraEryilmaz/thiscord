//! Scheduling class for video capture/encode and paced-send threads. Sharing a
//! game is the common case: at normal priority a saturated CPU can delay frame
//! submission and pacing wakes, turning into raw-frame drops and send bursts.
//!
//! Like the audio worker this is deliberately not a realtime class. MMCSS
//! "Capture" (and macOS user-interactive QoS) keeps the system responsive if a
//! thread overruns. Linux keeps the default policy (no rtkit dependency).

/// Applies once to the calling thread for its lifetime; returns a label.
pub fn promote_video_thread() -> &'static str {
    #[cfg(target_os = "windows")]
    {
        let mut task = 0u32;
        // SAFETY: a static NUL-terminated task name and a valid output pointer.
        // The returned handle is intentionally kept for the thread's lifetime.
        match unsafe {
            windows::Win32::System::Threading::AvSetMmThreadCharacteristicsW(
                windows::core::w!("Capture"),
                &mut task,
            )
        } {
            Ok(_) => "mmcss_capture",
            Err(_) => "default",
        }
    }
    #[cfg(target_os = "macos")]
    {
        unsafe extern "C" {
            fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
        // SAFETY: only changes the calling thread's QoS class.
        match unsafe { pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0) } {
            0 => "qos_user_interactive",
            _ => "default",
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        "default"
    }
}
