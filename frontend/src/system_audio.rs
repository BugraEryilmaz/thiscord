//! Windows process loopback excludes this process tree, including voice playback.
use crate::native_screen::{Binding, ScreenState};
use std::sync::{Arc, atomic::AtomicBool};

pub fn start(
    binding: Binding,
    stop: Arc<AtomicBool>,
    state: ScreenState,
) -> Result<std::thread::JoinHandle<()>, String> {
    std::thread::Builder::new()
        .name("thiscord-system-audio".into())
        .spawn(move || {
            if let Err(error) = capture(&binding, &stop) {
                state.finish(&stop, Some(error));
            }
        })
        .map_err(|_| "Cannot start system audio worker".into())
}
#[cfg(target_os = "windows")]
fn capture(binding: &Binding, stop: &AtomicBool) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    use std::{collections::VecDeque, time::Duration};
    use wasapi::*;
    use webrtc::media_stream::track_local::TrackLocal;
    initialize_mta()
        .ok()
        .map_err(|_| "Cannot initialize system audio")?;
    struct Com;
    impl Drop for Com {
        fn drop(&mut self) {
            deinitialize();
        }
    }
    let _com = Com;
    let mut client = AudioClient::new_application_loopback_client(std::process::id(), false)
        .map_err(|_| "System audio requires Windows build 20348 or newer (Windows 11). Video-only sharing is still available.")?;
    let format = WaveFormat::new(32, 32, &SampleType::Float, 48_000, 2, None);
    client
        .initialize_client(
            &format,
            &Direction::Capture,
            &StreamMode::EventsShared {
                autoconvert: true,
                buffer_duration_hns: 0,
            },
        )
        .map_err(|_| "Cannot open system audio capture")?;
    let event = client
        .set_get_eventhandle()
        .map_err(|_| "Cannot initialize system audio notifications")?;
    let capture = client
        .get_audiocaptureclient()
        .map_err(|_| "Cannot access system audio")?;
    let mut encoder = opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Audio)
        .map_err(|_| "Cannot initialize shared audio encoder")?;
    encoder
        .set_bitrate(opus::Bitrate::Bits(64_000))
        .map_err(|_| "Cannot configure shared audio encoder")?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|_| "Cannot initialize shared audio sender")?;
    let mut queue = VecDeque::<u8>::with_capacity(48_000);
    let mut pcm = [0.0_f32; 960];
    let mut encoded = [0_u8; 4000];
    client
        .start_stream()
        .map_err(|_| "Cannot start system audio capture")?;
    let mut previous = std::time::Instant::now();
    let result = (|| {
        while !stop.load(Ordering::Acquire) && binding.connection.active() {
            if previous.elapsed() > Duration::from_secs(2) {
                return Err("System audio paused; start sharing again.".into());
            }
            previous = std::time::Instant::now();
            let frames = capture
                .get_next_packet_size()
                .map_err(|_| "System audio capture was interrupted")?
                .unwrap_or(0) as usize;
            if frames > 6000 {
                return Err("System audio capture fell behind; sharing stopped".into());
            }
            if frames > 0 {
                if queue.len() + frames * 8 > 48_000 {
                    queue.clear();
                }
                capture
                    .read_from_device_to_deque(&mut queue)
                    .map_err(|_| "Cannot read system audio")?;
            }
            while queue.len() >= 960 * 8 {
                for sample in &mut pcm {
                    let mut pair = [0; 8];
                    for byte in &mut pair {
                        *byte = queue.pop_front().unwrap_or(0);
                    }
                    let left = f32::from_le_bytes(pair[..4].try_into().unwrap());
                    let right = f32::from_le_bytes(pair[4..].try_into().unwrap());
                    let mixed = (left + right) * 0.5;
                    *sample = if mixed.is_finite() {
                        mixed.clamp(-1.0, 1.0)
                    } else {
                        0.0
                    };
                }
                if stop.load(Ordering::Acquire) || !binding.connection.active() {
                    break;
                }
                let n = encoder
                    .encode_float(&pcm, &mut encoded)
                    .map_err(|_| "Cannot encode shared audio")?;
                let packet = rtc::rtp::Packet {
                    header: rtc::rtp::header::Header {
                        version: 2,
                        payload_type: 111,
                        ssrc: 902,
                        sequence_number: binding.audio_sequence.fetch_add(1, Ordering::Relaxed)
                            as u16,
                        timestamp: binding.audio_timestamp.fetch_add(960, Ordering::Relaxed),
                        ..Default::default()
                    },
                    payload: bytes::Bytes::copy_from_slice(&encoded[..n]),
                };
                if !matches!(
                    runtime.block_on(async {
                        tokio::time::timeout(
                            Duration::from_millis(100),
                            binding.audio.write_rtp(packet),
                        )
                        .await
                    }),
                    Ok(Ok(_))
                ) {
                    return Err("Shared audio sender stalled; sharing stopped".into());
                }
            }
            // Silence produces no loopback packets. Poll with a bounded wait so
            // Stop, disconnect and permission revocation promptly release WASAPI.
            let _ = event.wait_for_event(20);
        }
        Ok(())
    })();
    let _ = client.stop_stream();
    result
}
