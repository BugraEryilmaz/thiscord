//! Native microphone policy and queue metadata; never a Tauri/wire DTO.
use super::mixer::{FRAME, RATE};
use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

pub const MAX_AGE: Duration = Duration::from_millis(100);

/// A pending transport future must pass admission again on every poll. Dropping
/// it cannot retract data already accepted by the underlying transport.
pub async fn while_permitted<E>(
    write: impl std::future::Future<Output = Result<(), E>>,
    permitted: impl Fn() -> bool,
) -> Result<bool, E> {
    let mut write = std::pin::pin!(write);
    std::future::poll_fn(|cx| {
        if !permitted() {
            return std::task::Poll::Ready(Ok(false));
        }
        write.as_mut().poll(cx).map(|result| result.map(|_| true))
    })
    .await
}

pub struct Packet {
    pub payload: bytes::Bytes,
    pub timestamp: u32,
    pub marker: bool,
    pub captured: Instant,
    pub generation: u64,
}

/// Control threads and the audio worker only; device callbacks never touch this.
/// The cutoff survives release/repress even when neither reaches the worker.
pub struct Restrictions(Mutex<(u64, Instant)>);
impl Default for Restrictions {
    fn default() -> Self {
        Self(Mutex::new((0, Instant::now())))
    }
}
impl Restrictions {
    pub fn invalidate(&self) {
        let mut state = self.0.lock().unwrap_or_else(|e| e.into_inner());
        state.0 = state.0.wrapping_add(1);
        state.1 = Instant::now();
    }
    pub fn snapshot(&self) -> (u64, Instant) {
        *self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn accepts(&self, packet: &Packet, now: Instant) -> bool {
        let (generation, cutoff) = self.snapshot();
        packet.generation == generation
            && packet.captured >= cutoff
            && now.saturating_duration_since(packet.captured) <= MAX_AGE
    }
}

#[derive(Default, serde::Serialize)]
pub struct Stats {
    pub encoded_frames: u64,
    pub encoded_bytes: u64,
    pub omitted_packets: u64,
    pub omitted_bytes: u64,
    pub encode_ns: u64,
    pub queued_packets: u64,
    pub queued_bytes: u64,
    pub queue_drops: u64,
}

pub fn microphone_encoder(dtx: bool) -> Result<opus::Encoder, opus::Error> {
    let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)?;
    encoder.set_bitrate(opus::Bitrate::Bits(32_000))?;
    encoder.set_dtx(dtx)?;
    // Existing clients do not recover FEC. Keep it explicitly off: a resumed
    // packet must never repair a timestamp belonging to intentional silence.
    encoder.set_inband_fec(false)?;
    Ok(encoder)
}

#[derive(Default)]
pub struct Clock {
    previous: Option<(u64, Instant)>,
    timestamp: u32,
}
impl Clock {
    pub fn advance(&mut self, position: u64, captured: Instant) -> u32 {
        if let Some((previous, at)) = self.previous {
            // Sample positions survive ring overflow and callback batching. A
            // newly opened device resets its counter: bridge that interval with
            // the monotonic capture clock, without resetting the RTP timeline.
            let samples = if position > previous {
                position - previous
            } else {
                (captured.saturating_duration_since(at).as_secs_f64() * RATE as f64).round() as u64
            };
            self.timestamp = self
                .timestamp
                .wrapping_add(samples.max(FRAME as u64) as u32);
        }
        self.previous = Some((position, captured));
        self.timestamp
    }
}

pub struct Encoder {
    pub codec: opus::Encoder,
    pub stats: Stats,
    in_dtx: bool,
    dtx_enabled: bool,
}
impl Encoder {
    pub fn new(dtx: bool) -> Result<Self, opus::Error> {
        Ok(Self {
            codec: microphone_encoder(dtx)?,
            stats: Stats::default(),
            in_dtx: false,
            dtx_enabled: dtx,
        })
    }
    pub fn reset(&mut self) -> Result<(), opus::Error> {
        self.in_dtx = false;
        self.codec.reset_state()
    }
    pub fn encode(
        &mut self,
        pcm: &[f32; FRAME],
        protect_speech: bool,
        output: &mut [u8],
    ) -> Result<(usize, bool, bool), opus::Error> {
        let start = Instant::now();
        // Do not let codec VAD discard quiet speech/pre-roll admitted by our gate.
        self.codec.set_dtx(self.dtx_enabled && !protect_speech)?;
        let n = self.codec.encode_float(pcm, output)?;
        let dtx = self.codec.get_in_dtx()?;
        self.stats.encode_ns += start.elapsed().as_nanos() as u64;
        self.stats.encoded_frames += 1;
        self.stats.encoded_bytes += n as u64;
        // libopus 1.6.1 opus.h explicitly permits omitting <=2-byte results.
        // GET_IN_DTX alone is NOT sufficient: it includes mandatory CN updates.
        let omit = dtx && n <= 2;
        let marker = self.in_dtx && !dtx;
        self.in_dtx = dtx;
        if omit {
            self.stats.omitted_packets += 1;
            self.stats.omitted_bytes += n as u64;
        }
        Ok((n, omit, marker))
    }
}

/// One frame of look-behind protects a quiet onset before threshold crossing.
/// This adds 20 ms after DSP; AEC capture/render timestamps are unchanged.
#[derive(Default)]
pub struct Gate {
    previous: Option<[f32; FRAME]>,
    previous_active: bool,
}
impl Gate {
    pub fn process(
        &mut self,
        pcm: [f32; FRAME],
        active: bool,
        allowed: bool,
    ) -> ([f32; FRAME], bool) {
        if !allowed {
            self.clear();
            return ([0.0; FRAME], false);
        }
        let previous = self.previous.replace(pcm).unwrap_or([0.0; FRAME]);
        let transmit = self.previous_active || active;
        self.previous_active = active;
        (if transmit { previous } else { [0.0; FRAME] }, transmit)
    }
    pub fn clear(&mut self) {
        self.previous = None;
        self.previous_active = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pending_transport_does_not_resume_after_restriction_or_connection_change() {
        use std::{
            cell::Cell,
            future::Future,
            task::{Context, Poll, Waker},
        };
        let allowed = Cell::new(true);
        let polls = Cell::new(0);
        let write = std::future::poll_fn(|_| {
            polls.set(polls.get() + 1);
            Poll::<Result<(), ()>>::Pending
        });
        let mut admitted = std::pin::pin!(while_permitted(write, || allowed.get()));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(admitted.as_mut().poll(&mut cx).is_pending());
        allowed.set(false);
        assert_eq!(admitted.as_mut().poll(&mut cx), Poll::Ready(Ok(false)));
        assert_eq!(polls.get(), 1);
    }
    #[test]
    fn real_opus_silence_keeps_updates_and_protects_quiet_and_abrupt_onsets() {
        let mut encoder = Encoder::new(true).unwrap();
        assert!(!encoder.codec.get_inband_fec().unwrap());
        let mut output = [0; 4000];
        let mut last_sent = 0;
        for frame in 0..3000 {
            let (n, omitted, _) = encoder.encode(&[0.0; FRAME], false, &mut output).unwrap();
            if !omitted {
                last_sent = frame;
                assert!(n > 2);
            }
            assert!(
                frame - last_sent <= 21,
                "comfort-noise updates must survive"
            );
        }
        assert!(encoder.stats.omitted_packets > 2800);
        // Silence DTX may not second-guess either quiet pre-roll or loud onset.
        for level in [0.0001, 0.2] {
            let pcm = std::array::from_fn(|i| (i as f32 * 0.04).sin() * level);
            let (_, omitted, _) = encoder.encode(&pcm, true, &mut output).unwrap();
            assert!(!omitted);
        }
        let mut continuous = Encoder::new(false).unwrap();
        for _ in 0..100 {
            assert!(
                !continuous
                    .encode(&[0.0; FRAME], false, &mut output)
                    .unwrap()
                    .1
            );
        }
    }
    #[test]
    fn gate_retains_quiet_preroll_but_restrictions_clear_it() {
        let mut gate = Gate::default();
        assert_eq!(gate.process([0.001; FRAME], false, true).0, [0.0; FRAME]);
        assert_eq!(
            gate.process([0.2; FRAME], true, true),
            ([0.001; FRAME], true)
        );
        assert_eq!(
            gate.process([0.3; FRAME], true, false),
            ([0.0; FRAME], false)
        );
        assert_eq!(gate.process([0.4; FRAME], true, true).0, [0.0; FRAME]);
        assert_eq!(gate.process([0.0; FRAME], false, true).0, [0.4; FRAME]);
        gate.clear();
        assert_eq!(gate.process([0.0; FRAME], true, true).0, [0.0; FRAME]);
    }
    #[test]
    fn clock_advances_through_omission_gaps_recovery_and_wrap() {
        let now = Instant::now();
        let mut clock = Clock {
            previous: None,
            timestamp: u32::MAX - 959,
        };
        assert_eq!(clock.advance(0, now), u32::MAX - 959);
        assert_eq!(clock.advance(960, now + Duration::from_millis(20)), 0);
        assert_eq!(clock.advance(0, now + Duration::from_secs(60)), 2_879_040);
        assert_eq!(
            clock.advance(960, now + Duration::from_millis(60_020)),
            2_880_000
        );
        // Callback wake timing is irrelevant to ordinary media progression.
        assert_eq!(
            clock.advance(2400, now + Duration::from_millis(60_050)),
            2_881_440
        );
    }
    #[test]
    fn restriction_epoch_and_age_reject_retained_audio() {
        let restrictions = Restrictions::default();
        let mut packet = Packet {
            payload: bytes::Bytes::new(),
            timestamp: 0,
            marker: false,
            captured: Instant::now(),
            generation: 0,
        };
        assert!(restrictions.accepts(&packet, packet.captured));
        assert!(
            !restrictions.accepts(&packet, packet.captured + MAX_AGE + Duration::from_nanos(1))
        );
        restrictions.invalidate(); // release/mute
        restrictions.invalidate(); // repress/unmute, even without a worker tick
        assert!(!restrictions.accepts(&packet, Instant::now()));
        packet.generation = restrictions.snapshot().0;
        assert!(
            !restrictions.accepts(&packet, Instant::now()),
            "old capture is still stale"
        );
        packet.captured = Instant::now();
        assert!(restrictions.accepts(&packet, packet.captured));
    }
}
