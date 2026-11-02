//! Linux screen sharing: xdg-desktop-portal ScreenCast selects the source and
//! hands over a PipeWire remote; GStreamer copies each frame into GPU memory
//! of the selected encoder family (VA-API, NVENC) and encodes H.264 on the
//! same device. Admission, adaptation, layering and pacing are the shared
//! `worker` loop used on every platform. System audio comes from PipeWire.
mod audio;
mod media;
mod portal;

pub use audio::system_audio;

use super::{
    cadence::Cadence,
    metrics::Metrics,
    worker::{self, Layer, Pixels, Shared, Stops, capture_fps},
};
use crate::audio::connection::Connection;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU32, Ordering},
    },
    time::{Duration, Instant},
};
use thiscord_shared::screen::{Quality, Source, SourceId};

/// The portal owns selection; the two entries pick its Screens/Windows mode.
pub fn sources() -> Result<Vec<Source>, String> {
    media::installed()?;
    let (screens, windows) = portal::available()?;
    if !screens && !windows {
        return Err(portal::UNAVAILABLE.into());
    }
    let mut sources = Vec::new();
    if screens {
        sources.push(Source {
            id: SourceId::Monitor(0),
            label: "Screen — choose in system dialog".into(),
        });
    }
    if windows {
        sources.push(Source {
            id: SourceId::Window(0),
            label: "Window — choose in system dialog".into(),
        });
    }
    Ok(sources)
}

#[allow(clippy::too_many_arguments)]
pub fn run(
    source: &SourceId,
    quality: Quality,
    stop: Arc<AtomicBool>,
    connection: Connection,
    started: Instant,
    tx: tokio::sync::mpsc::Sender<super::Outgoing>,
    metrics: Metrics,
    force_keyframe: Arc<AtomicBool>,
    demand: Arc<super::adaptation::Demand>,
    report: impl FnMut(String),
) -> Result<(), String> {
    run_layers(
        source,
        quality,
        stop,
        connection,
        started,
        tx,
        metrics,
        force_keyframe,
        demand,
        None,
        Arc::default(),
        report,
    )
}

/// `high_profile` is one stream-wide flag for both layers (see `VideoProfile`).
#[allow(clippy::too_many_arguments)]
pub fn run_layers(
    source: &SourceId,
    quality: Quality,
    stop: Arc<AtomicBool>,
    connection: Connection,
    started: Instant,
    tx: tokio::sync::mpsc::Sender<super::Outgoing>,
    metrics: Metrics,
    force_keyframe: Arc<AtomicBool>,
    demand: Arc<super::adaptation::Demand>,
    low: Option<super::LayerOutput>,
    high_profile: Arc<AtomicBool>,
    mut report: impl FnMut(String),
) -> Result<(), String> {
    media::init()?;
    let spec = media::select()?;
    metrics.label("encoder_family", spec.family.label());
    metrics.label(
        "capture_format",
        if spec.family.gpu() && !matches!(spec.family, media::Family::NvencCpuScale) {
            "pipewire_gpu_copy"
        } else {
            "pipewire_system_memory"
        },
    );
    let portal = portal::Portal::open(matches!(source, SourceId::Window(_)), &stop)?;
    if stop.load(Ordering::Acquire) || !connection.active() {
        return Ok(());
    }
    let mailbox = worker::inbox();
    let low_mailbox = low.as_ref().map(|_| worker::inbox());
    let mut mailboxes = vec![mailbox.clone()];
    mailboxes.extend(low_mailbox.iter().cloned());
    let active_fps: Arc<[AtomicU32; 2]> = Arc::new(std::array::from_fn(|_| AtomicU32::new(0)));
    let session_stop = Arc::new(AtomicBool::new(false));
    let capture = media::Capture::start(spec, portal.fd(), portal.node(), {
        let (stop, session, connection, active_fps, metrics) = (
            stop.clone(),
            session_stop.clone(),
            connection.clone(),
            active_fps.clone(),
            metrics.clone(),
        );
        let period = Duration::from_secs_f64(1.0 / f64::from(quality.fps));
        let mut throttle = (period, Cadence::new(period));
        move |frame: media::Frame| {
            if stop.load(Ordering::Acquire)
                || session.load(Ordering::Acquire)
                || !connection.active()
            {
                return;
            }
            let period = Duration::from_secs_f64(1.0 / f64::from(capture_fps(&active_fps)));
            if period != throttle.0 {
                throttle = (period, Cadence::new(period));
            }
            let now = Instant::now();
            metrics.add("capture_events", 1);
            // Rate-limit work on compositor events, never sleep in the callback.
            if !throttle.1.accept(now) {
                metrics.add("capture_throttled", 1);
                return;
            }
            let Some((width, height)) = frame.size() else {
                return;
            };
            if !valid_size(width, height) {
                metrics.add("capture_unsupported_size", 1);
                return;
            }
            worker::deliver(
                &mailboxes,
                Pixels {
                    frame,
                    width,
                    height,
                    arrived: now,
                },
                &metrics,
            );
            metrics.time("capture_submit", now.elapsed());
        }
    })?;
    let stops = Stops {
        user: &stop,
        session: &session_stop,
    };
    let shared = Shared {
        stops: &stops,
        connection: &connection,
        started,
        active_fps: &active_fps,
        high_profile: &high_profile,
    };
    let contexts = Mutex::new(None);
    let platform = || Linux {
        spec,
        capture: &capture,
        portal: &portal,
        contexts: &contexts,
    };
    let finished = || capture.failed();
    let result = worker::run_layers(
        &mut platform(),
        &mailbox,
        Some(&finished),
        low.as_ref()
            .zip(low_mailbox)
            .map(|(low, mailbox)| (platform(), mailbox, low)),
        &shared,
        Layer {
            quality,
            tx,
            metrics: &metrics,
            force_keyframe: &force_keyframe,
            demand: &demand,
            secondary: false,
        },
        &stop,
        &mut report,
    );
    let failure = capture.failure();
    drop(capture);
    drop(portal);
    result.map(|_| ()).map_err(|error| match failure {
        // A concrete PipeWire/GStreamer error explains "source closed".
        Some(cause) if error.contains("source closed") => {
            format!("Screen capture stopped ({cause}); sharing stopped.")
        }
        _ => error,
    })
}

fn valid_size(width: u32, height: u32) -> bool {
    (2..=16384).contains(&width)
        && (2..=16384).contains(&height)
        && u64::from(width) * u64::from(height) <= 33_554_432
}

/// Linux layer hooks: portal liveness and GStreamer encoder construction.
struct Linux<'a> {
    spec: &'static media::Spec,
    capture: &'a media::Capture,
    portal: &'a portal::Portal,
    /// Capture device contexts, read once a frame has been negotiated.
    contexts: &'a Mutex<Option<Vec<gstreamer::Context>>>,
}
impl worker::Platform for Linux<'_> {
    type Frame = media::Frame;
    type Encoder = media::Layer;
    fn source_lost(&mut self) -> bool {
        self.portal.closed()
    }
    fn encoder(
        &mut self,
        pixels: &Pixels<media::Frame>,
        size: (u32, u32),
        quality: Quality,
        options: super::encode::EncoderOptions,
        wake: Box<dyn Fn() + Send + Sync>,
        metrics: &Metrics,
    ) -> Result<media::Layer, String> {
        let contexts = self
            .contexts
            .lock()
            .unwrap()
            .get_or_insert_with(|| self.capture.contexts(self.spec))
            .clone();
        metrics.label("encoder_family", self.spec.family.label());
        metrics.label("pixel_path", self.spec.pixel_path);
        metrics.label(
            "encoder_buffer_target_bytes",
            super::encode::buffer_bytes(quality.bitrate()),
        );
        metrics.label("encoder_shared_contexts", contexts.len());
        if !self.spec.family.gpu() {
            metrics.event("no GPU H.264 encoder available; using software encoding");
        }
        media::Layer::new(
            self.spec,
            &pixels.frame.caps,
            size,
            quality,
            options,
            &contexts,
            wake,
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn capture_rejects_unbounded_sources() {
        assert!(super::valid_size(3840, 2160));
        assert!(super::valid_size(7680, 4320));
        assert!(!super::valid_size(1, 1080));
        assert!(!super::valid_size(16385, 100));
        assert!(!super::valid_size(16384, 16384));
    }
}
