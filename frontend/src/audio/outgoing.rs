//! Native microphone timing and bounded transport admission. Never sent over IPC.
use super::{connection::Connection, frames, mixer::RATE};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

pub const QUEUE_CAPACITY: usize = 5;
/// Includes acquisition, enhancement, encoding and queueing. The fixed activation
/// lookbehind has a separate bounded allowance, so DFN3 + pre-roll stays usable.
pub const MAX_PACKET_AGE: Duration = Duration::from_millis(100);

pub struct EncodedFrame {
    pub payload: bytes::Bytes,
    pub media_position: u64,
    pub oldest_capture: Instant,
    pub generation: u64,
    pub connection: Connection,
    pub delivery: Connection,
    pub activation_delay: Duration,
}
impl EncodedFrame {
    fn deadline(&self) -> Instant {
        self.oldest_capture + MAX_PACKET_AGE + self.activation_delay.min(super::gate::DELAY)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DropReason {
    QueueFull,
    Expired,
    ControlInvalidated,
}

/// One atomic cutoff is both an invalidation generation and a capture-time fence.
/// A worker cannot stamp pre-mute PCM with a fresh generation after unmuting.
pub struct Control {
    changed: tokio::sync::Notify,
    origin: Instant,
    cutoff: AtomicU64,
    queue_full: AtomicU64,
    expired: AtomicU64,
    invalidated: AtomicU64,
}
impl Default for Control {
    fn default() -> Self {
        Self::new(Instant::now())
    }
}
impl Control {
    fn new(origin: Instant) -> Self {
        Self {
            changed: Default::default(),
            origin,
            cutoff: AtomicU64::new(0),
            queue_full: AtomicU64::new(0),
            expired: AtomicU64::new(0),
            invalidated: AtomicU64::new(0),
        }
    }
    pub fn invalidate(&self) {
        self.invalidate_at(Instant::now());
    }
    pub(super) fn cancel_pending(&self) {
        self.changed.notify_waiters();
    }
    fn invalidate_at(&self, now: Instant) {
        let stamp = now.saturating_duration_since(self.origin).as_nanos() as u64 + 1;
        let _ = self
            .cutoff
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |old| {
                Some(stamp.max(old.saturating_add(1)))
            });
        self.changed.notify_waiters();
    }
    pub fn generation(&self) -> u64 {
        self.cutoff.load(Ordering::Acquire)
    }
    pub fn fresh_capture(&self, generation: u64, oldest: Instant) -> bool {
        generation == self.generation()
            && (generation == 0 || oldest > self.origin + Duration::from_nanos(generation - 1))
    }
    pub fn check(
        &self,
        frame: &EncodedFrame,
        connection: &Connection,
        now: Instant,
    ) -> Result<(), DropReason> {
        if !connection.accepts(&frame.connection)
            || !frame.delivery.active()
            || frame.generation != self.generation()
        {
            return Err(DropReason::ControlInvalidated);
        }
        if now >= frame.deadline() {
            return Err(DropReason::Expired);
        }
        Ok(())
    }
    /// Revalidate on EVERY transport poll, including after internal lock/queue
    /// waits. Cancellation cannot recall a packet already admitted by WebRTC.
    pub async fn guard_write<F: std::future::Future>(
        &self,
        frame: &EncodedFrame,
        connection: &Connection,
        write: F,
    ) -> Result<F::Output, DropReason> {
        self.guard_write_with_clock(frame, connection, write, Instant::now)
            .await
    }
    async fn guard_write_with_clock<F: std::future::Future>(
        &self,
        frame: &EncodedFrame,
        connection: &Connection,
        write: F,
        now: impl Fn() -> Instant,
    ) -> Result<F::Output, DropReason> {
        let changed = self.changed.notified();
        tokio::pin!(changed);
        changed.as_mut().enable();
        tokio::pin!(write);
        let guarded = std::future::poll_fn(|cx| {
            if let Err(reason) = self.check(frame, connection, now()) {
                return std::task::Poll::Ready(Err(reason));
            }
            write.as_mut().poll(cx).map(Ok)
        });
        tokio::select! {
            biased;
            result = guarded => result,
            _ = &mut changed => Err(DropReason::ControlInvalidated),
            _ = tokio::time::sleep_until(frame.deadline().into()) => Err(DropReason::Expired),
        }
    }
    pub fn dropped(&self, reason: DropReason) {
        match reason {
            DropReason::QueueFull => &self.queue_full,
            DropReason::Expired => &self.expired,
            DropReason::ControlInvalidated => &self.invalidated,
        }
        .fetch_add(1, Ordering::Relaxed);
    }
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "queue_full": self.queue_full.load(Ordering::Relaxed),
            "expired": self.expired.load(Ordering::Relaxed),
            "control_invalidated": self.invalidated.load(Ordering::Relaxed),
        })
    }
}

/// Sample-count clock within a device stream; monotonic Instant bridge on reopen.
/// Queue rejection never changes this clock. It belongs to one audio session.
#[derive(Default)]
pub(super) struct MediaClock {
    epoch: Option<Instant>,
    source: Option<(u64, u64)>,
    next: u64,
}
impl MediaClock {
    pub fn restart_device(&mut self) {
        self.source = None;
    }
    pub fn position(&mut self, at: Instant, sample: u64) -> u64 {
        let epoch = *self.epoch.get_or_insert(at);
        let (source_sample, source_media) = *self.source.get_or_insert_with(|| {
            let elapsed = samples(at.saturating_duration_since(epoch));
            (sample, elapsed.max(self.next))
        });
        let position = source_media
            .saturating_add(sample.saturating_sub(source_sample))
            .max(self.next);
        self.next = position.saturating_add(960);
        position
    }
}

pub(super) fn samples(duration: Duration) -> u64 {
    ((duration.as_nanos() * u128::from(RATE) + 500_000_000) / 1_000_000_000)
        .min(u128::from(u64::MAX)) as u64
}

pub(super) fn oldest_capture(at: Instant, delay_samples: usize) -> Instant {
    at.checked_sub(frames::sample_duration(delay_samples))
        .unwrap_or(at)
}

/// Sequence numbers count transport attempts, independently of capture time.
#[derive(Default)]
pub struct RtpClock {
    sequence: u16,
}
impl RtpClock {
    pub fn next(&mut self, media_position: u64) -> (u16, u32) {
        self.sequence = self.sequence.wrapping_add(1);
        (self.sequence, media_position as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(
        at: Instant,
        generation: u64,
        connection: &Connection,
        media_position: u64,
    ) -> EncodedFrame {
        EncodedFrame {
            payload: bytes::Bytes::from_static(b"synthetic"),
            oldest_capture: at,
            generation,
            connection: connection.clone(),
            delivery: Connection::default(),
            activation_delay: Duration::ZERO,
            media_position,
        }
    }

    #[test]
    fn normal_packets_and_queue_drops_keep_sample_time_independent_of_sequence() {
        let at = Instant::now();
        let mut media = MediaClock::default();
        let mut rtp = RtpClock::default();
        let connection = Connection::default();
        let control = Control::new(at);
        let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE_CAPACITY);
        for i in 0..10u64 {
            let position = media.position(at + frames::sample_duration(i as usize * 960), i * 960);
            assert_eq!(position, i * 960);
            if tx.try_send(frame(at, 0, &connection, position)).is_err() {
                control.dropped(DropReason::QueueFull);
            }
        }
        for i in 0..QUEUE_CAPACITY {
            let queued = rx.try_recv().unwrap();
            assert_eq!(
                rtp.next(queued.media_position),
                (i as u16 + 1, i as u32 * 960)
            );
        }
        let position = media.position(at + Duration::from_millis(200), 9600);
        assert_eq!(rtp.next(position), (6, 9600));
        assert_eq!(control.snapshot()["queue_full"], 5);
        // One skipped frame, followed by another live frame.
        media.position(at + Duration::from_millis(220), 10560);
        assert_eq!(
            rtp.next(media.position(at + Duration::from_millis(240), 11520)),
            (7, 11520)
        );
    }

    #[test]
    fn capture_overflow_and_recovery_keep_monotonic_connection_clock() {
        let at = Instant::now();
        let mut media = MediaClock::default();
        assert_eq!(media.position(at, 0), 0);
        assert_eq!(media.position(at + Duration::from_millis(50), 2400), 2400);
        media.restart_device();
        assert_eq!(media.position(at + Duration::from_secs(2), 0), 96000);
        assert_eq!(media.position(at + Duration::from_millis(2020), 960), 96960);
        // A regressing device timestamp cannot rewind the clock on reopen.
        media.restart_device();
        assert_eq!(media.position(at, 0), 97920);
        let mut replacement = MediaClock::default();
        assert_eq!(replacement.position(at + Duration::from_secs(3), 0), 0);
    }

    #[test]
    fn media_and_sequence_wrap_independently() {
        let mut rtp = RtpClock {
            sequence: u16::MAX - 1,
        };
        let position = u64::from(u32::MAX) - 479;
        assert_eq!(rtp.next(position), (u16::MAX, u32::MAX - 479));
        assert_eq!(rtp.next(position + 960), (0, 480));
        assert_eq!(rtp.next(position + 2880), (1, 2400));
    }

    #[test]
    fn stalled_sender_discards_backlog_and_resumes_at_live_edge_with_bounded_memory() {
        let at = Instant::now();
        let control = Control::new(at);
        let connection = Connection::default();
        let (tx, mut rx) = tokio::sync::mpsc::channel(QUEUE_CAPACITY);
        for i in 0..10_000 {
            if tx
                .try_send(frame(
                    at + Duration::from_millis(i * 20),
                    0,
                    &connection,
                    i * 960,
                ))
                .is_err()
            {
                control.dropped(DropReason::QueueFull);
            }
            assert!(rx.len() <= QUEUE_CAPACITY);
        }
        let now = at + Duration::from_secs(200);
        for _ in 0..QUEUE_CAPACITY {
            let old = rx.try_recv().unwrap();
            let reason = control.check(&old, &connection, now).unwrap_err();
            assert_eq!(reason, DropReason::Expired);
            control.dropped(reason);
        }
        assert!(rx.try_recv().is_err());
        tx.try_send(frame(
            now - Duration::from_millis(20),
            0,
            &connection,
            9_599_040,
        ))
        .ok()
        .unwrap();
        assert!(
            control
                .check(&rx.try_recv().unwrap(), &connection, now)
                .is_ok()
        );
        assert_eq!(
            control.snapshot(),
            serde_json::json!({"queue_full":9995,"expired":5,"control_invalidated":0})
        );
    }

    #[test]
    fn age_includes_explicit_dsp_delay_and_boundary_is_exclusive() {
        let at = Instant::now();
        let control = Control::new(at);
        let connection = Connection::default();
        let f = frame(oldest_capture(at, 1440), 0, &connection, 0);
        assert!(
            control
                .check(&f, &connection, at + Duration::from_millis(69))
                .is_ok()
        );
        assert_eq!(
            control.check(&f, &connection, at + Duration::from_millis(70)),
            Err(DropReason::Expired)
        );
    }

    #[test]
    fn preroll_allowance_preserves_dsp_budget_without_allowing_unbounded_age() {
        let at = Instant::now();
        let control = Control::new(at);
        let connection = Connection::default();
        // Current output slot starts 90 ms after its oldest source sample:
        // 30 ms DFN alignment + 60 ms activation lookbehind.
        let mut f = frame(oldest_capture(at, 1440 + 2880), 0, &connection, 0);
        f.activation_delay = super::super::gate::DELAY;
        assert!(
            control
                .check(&f, &connection, at + Duration::from_millis(20))
                .is_ok()
        );
        assert!(
            control
                .check(&f, &connection, at + Duration::from_millis(69))
                .is_ok()
        );
        assert_eq!(
            control.check(&f, &connection, at + Duration::from_millis(70)),
            Err(DropReason::Expired)
        );
        // Even incorrectly supplied metadata cannot grant a larger allowance.
        f.activation_delay = Duration::from_secs(10);
        assert_eq!(
            control.check(&f, &connection, at + Duration::from_millis(70)),
            Err(DropReason::Expired)
        );
        f.activation_delay = Duration::ZERO;
        assert_eq!(
            control.check(&f, &connection, at + Duration::from_millis(20)),
            Err(DropReason::Expired)
        );
    }

    #[tokio::test]
    async fn gap_delivery_invalidation_cancels_pending_write_without_changing_control_generation() {
        let at = Instant::now();
        let control = Control::new(at);
        let connection = Connection::default();
        let f = frame(at, 0, &connection, 0);
        let write = std::future::poll_fn(|_| {
            f.delivery.close();
            control.cancel_pending();
            std::task::Poll::<()>::Pending
        });
        assert_eq!(
            control.guard_write(&f, &connection, write).await,
            Err(DropReason::ControlInvalidated)
        );
        assert_eq!(control.generation(), 0);
        assert!(connection.active());
    }

    #[tokio::test]
    async fn pending_preroll_write_expires_at_the_same_bounded_deadline() {
        let at = Instant::now();
        let control = Control::new(at);
        let connection = Connection::default();
        let mut f = frame(at, 0, &connection, 0);
        f.activation_delay = super::super::gate::DELAY;
        let now = std::cell::Cell::new(at + Duration::from_millis(110));
        let polls = std::cell::Cell::new(0);
        let write = std::future::poll_fn(|cx| {
            polls.set(polls.get() + 1);
            now.set(at + Duration::from_millis(160));
            cx.waker().wake_by_ref();
            std::task::Poll::<()>::Pending
        });
        assert_eq!(
            control
                .guard_write_with_clock(&f, &connection, write, || now.get())
                .await,
            Err(DropReason::Expired)
        );
        assert_eq!(polls.get(), 1);
    }

    #[test]
    fn generation_and_capture_fence_survive_relaxation_and_replacement() {
        let at = Instant::now();
        let control = Control::new(at);
        let connection = Connection::default();
        let old = frame(at, 0, &connection, 0);
        control.invalidate_at(at + Duration::from_millis(20));
        let generation = control.generation();
        assert_eq!(
            control.check(&old, &connection, at),
            Err(DropReason::ControlInvalidated)
        );
        assert!(!control.fresh_capture(generation, at));
        assert!(!control.fresh_capture(
            generation,
            oldest_capture(at + Duration::from_millis(40), 1440)
        ));
        assert!(control.fresh_capture(generation, at + Duration::from_millis(21)));
        let fresh = frame(at + Duration::from_millis(21), generation, &connection, 960);
        assert!(control.check(&fresh, &connection, at).is_ok());
        assert_eq!(
            control.check(&fresh, &Connection::default(), at),
            Err(DropReason::ControlInvalidated)
        );
        connection.close();
        assert_eq!(
            control.check(&fresh, &connection, at),
            Err(DropReason::ControlInvalidated)
        );
    }

    #[tokio::test]
    async fn waiting_transport_is_rechecked_before_it_can_admit_invalidated_audio() {
        let at = Instant::now();
        let control = Control::new(at);
        let connection = Connection::default();
        let f = frame(at, 0, &connection, 0);
        let polls = std::cell::Cell::new(0);
        let write = std::future::poll_fn(|cx| {
            polls.set(polls.get() + 1);
            control.invalidate_at(at);
            cx.waker().wake_by_ref();
            std::task::Poll::<()>::Pending
        });
        assert_eq!(
            control.guard_write(&f, &connection, write).await,
            Err(DropReason::ControlInvalidated)
        );
        assert_eq!(polls.get(), 1);
    }

    #[tokio::test]
    async fn expiration_during_transport_wait_prevents_a_second_admission_poll() {
        let at = Instant::now();
        let control = Control::new(at);
        let connection = Connection::default();
        let f = frame(at, 0, &connection, 0);
        let now = std::cell::Cell::new(at);
        let polls = std::cell::Cell::new(0);
        let write = std::future::poll_fn(|cx| {
            polls.set(polls.get() + 1);
            now.set(at + MAX_PACKET_AGE);
            cx.waker().wake_by_ref();
            std::task::Poll::<()>::Pending
        });
        assert_eq!(
            control
                .guard_write_with_clock(&f, &connection, write, || now.get())
                .await,
            Err(DropReason::Expired)
        );
        assert_eq!(polls.get(), 1);
    }
}
