//! Shared system audio. Windows process loopback excludes this process tree,
//! including voice playback. macOS uses ScreenCaptureKit audio excluding this
//! process; Linux routes every other application's PipeWire output into a
//! private capture sink. Capture threads never encode: Opus and RTP run here.
use crate::native_screen::{Binding, ScreenState};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use webrtc::media_stream::track_local::TrackLocal;

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
/// 48 kHz mono Opus at 64 kbit/s in 20 ms RTP packets on the shared-audio track.
struct Uplink {
    encoder: opus::Encoder,
    runtime: tokio::runtime::Runtime,
    encoded: [u8; 4000],
    /// Pooled payload storage: no allocation per 20 ms packet.
    arena: thiscord_frontend::audio::arena::PacketArena,
    pcm: [f32; 960],
    fill: usize,
}
impl Uplink {
    fn new() -> Result<Self, String> {
        let mut encoder =
            opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Audio)
                .map_err(|_| "Cannot initialize shared audio encoder")?;
        // Shared music/system audio keeps continuous transmission.
        encoder.set_dtx(false).map_err(|e| e.to_string())?;
        encoder
            .set_bitrate(opus::Bitrate::Bits(64_000))
            .map_err(|_| "Cannot configure shared audio encoder")?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "Cannot initialize shared audio sender")?;
        Ok(Self {
            encoder,
            runtime,
            encoded: [0; 4000],
            arena: Default::default(),
            pcm: [0.0; 960],
            fill: 0,
        })
    }
    /// Interleaved 48 kHz stereo in; every complete 20 ms downmix is sent.
    #[cfg_attr(
        target_os = "windows",
        expect(dead_code, reason = "WASAPI fills the downmix buffer directly.")
    )]
    fn push_stereo(
        &mut self,
        binding: &Binding,
        stop: &AtomicBool,
        samples: &[f32],
    ) -> Result<(), String> {
        for pair in samples.as_chunks::<2>().0 {
            self.pcm[self.fill] = downmix(pair[0], pair[1]);
            self.fill += 1;
            if self.fill == self.pcm.len() {
                self.fill = 0;
                if stop.load(Ordering::Acquire) || !binding.connection.active() {
                    return Ok(());
                }
                self.send(binding)?;
            }
        }
        Ok(())
    }
    fn send(&mut self, binding: &Binding) -> Result<(), String> {
        let n = self
            .encoder
            .encode_float(&self.pcm, &mut self.encoded)
            .map_err(|_| "Cannot encode shared audio")?;
        let packet = rtc::rtp::Packet {
            header: rtc::rtp::header::Header {
                version: 2,
                payload_type: thiscord_shared::voice::MediaKind::SystemAudio.payload_type(),
                ssrc: thiscord_shared::voice::MediaKind::SystemAudio.publisher_ssrc(),
                sequence_number: binding.audio_sequence.fetch_add(1, Ordering::Relaxed) as u16,
                timestamp: binding.audio_timestamp.fetch_add(960, Ordering::Relaxed),
                ..Default::default()
            },
            payload: self.arena.copy(&self.encoded[..n]),
        };
        if !matches!(
            self.runtime
                .block_on(thiscord_frontend::screen::ready_or_timeout(
                    binding.audio.write_rtp(packet),
                    Duration::from_millis(100),
                )),
            Some(Ok(_))
        ) {
            binding.metrics.add("audio_send_failures", 1);
            return Err("Shared audio sender stalled; sharing stopped".into());
        }
        binding.metrics.add("audio_sent_frames", 1);
        Ok(())
    }
}
fn downmix(left: f32, right: f32) -> f32 {
    let mixed = (left + right) * 0.5;
    if mixed.is_finite() {
        mixed.clamp(-1.0, 1.0)
    } else {
        0.0
    }
}

/// macOS/Linux capture lives with the platform screen backend; this worker
/// only receives interleaved stereo PCM and encodes it off capture threads.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn capture(binding: &Binding, stop: &AtomicBool) -> Result<(), String> {
    let mut uplink = Uplink::new()?;
    thiscord_frontend::screen::capture::system_audio(
        stop,
        &|| binding.connection.active(),
        &binding.metrics,
        &mut |samples| uplink.push_stereo(binding, stop, samples),
    )
}

#[cfg(target_os = "windows")]
fn capture(binding: &Binding, stop: &AtomicBool) -> Result<(), String> {
    use wasapi::*;
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
    let mut uplink = Uplink::new()?;
    // Contiguous byte queue: WASAPI copies each packet in once and whole 20 ms
    // chunks are downmixed in place (no per-byte deque pushes or pops).
    const QUEUE_BYTES: usize = 48_000;
    const CHUNK: usize = 960 * 8;
    let mut queue = vec![0_u8; QUEUE_BYTES * 2];
    let mut fill = 0;
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
                if fill + frames * 8 > QUEUE_BYTES {
                    fill = 0;
                    binding.metrics.add("audio_capture_queue_resets", 1);
                }
                let old_len = fill;
                // `fill <= QUEUE_BYTES`, so at least 6000 frames always fit.
                let (read, info) = capture
                    .read_from_device(&mut queue[fill..])
                    .map_err(|_| "Cannot read system audio")?;
                fill += read as usize * 8;
                binding.metrics.add("audio_capture_packets", 1);
                if info.flags.data_discontinuity {
                    binding.metrics.add("audio_capture_discontinuities", 1);
                }
                if info.flags.silent {
                    // WASAPI marks silence explicitly; do not encode the
                    // unspecified sample contents returned with this flag.
                    queue[old_len..fill].fill(0);
                    binding.metrics.add("audio_capture_silent_packets", 1);
                }
            }
            let mut consumed = 0;
            while fill - consumed >= CHUNK {
                let chunk = &queue[consumed..consumed + CHUNK];
                for (sample, pair) in uplink.pcm.iter_mut().zip(chunk.as_chunks::<8>().0) {
                    let left = f32::from_le_bytes([pair[0], pair[1], pair[2], pair[3]]);
                    let right = f32::from_le_bytes([pair[4], pair[5], pair[6], pair[7]]);
                    *sample = downmix(left, right);
                }
                consumed += CHUNK;
                if stop.load(Ordering::Acquire) || !binding.connection.active() {
                    break;
                }
                uplink.send(binding)?;
            }
            // Keep the partial tail (< 20 ms) at the front.
            queue.copy_within(consumed..fill, 0);
            fill -= consumed;
            // Silence produces no loopback packets. Poll with a bounded wait so
            // Stop, disconnect and permission revocation promptly release WASAPI.
            // Events can coalesce while this worker is encoding/sending. Drain
            // every already-available WASAPI packet before waiting for another
            // notification; one read per event strands captured audio.
            if capture
                .get_next_packet_size()
                .map_err(|_| "System audio capture was interrupted")?
                .unwrap_or(0)
                == 0
            {
                let _ = event.wait_for_event(20);
            }
        }
        Ok(())
    })();
    let _ = client.stop_stream();
    result
}
