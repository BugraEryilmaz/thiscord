//! Byte pacing with a small bounded burst and no catch-up after a scheduler stall.
use std::{
    sync::{
        Arc,
        atomic::{AtomicU32, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub fn transport_bitrate(codec_bitrate: u32) -> u32 {
    codec_bitrate * 12 / 10
}

/// Pace at available demand, independently of nominal codec output. Intra bursts
/// do not shrink proportionally with resolution/FPS: lowering a recovery mode
/// must not also take away the bandwidth needed to deliver its IDR.
pub fn transport_budget(_encoder_bitrate: u32, demand: u32) -> u32 {
    transport_bitrate(demand)
}

/// Count unsent wire bytes, including the frame currently being paced. Frame
/// count alone cannot distinguish a small delta from a 500 KB scene change.
/// This is raw-input admission, not an encoded-frame size/drop limit.
pub const BACKLOG_BUDGET_MS: u32 = 100;
type Wake = Arc<dyn Fn() + Send + Sync>;
/// The wake callback plus whether the encoder worker is currently held by
/// backpressure; frames finishing while nobody waits wake no one.
#[derive(Clone)]
struct Waiter {
    waiting: Arc<std::sync::atomic::AtomicBool>,
    wake: Wake,
}
impl Waiter {
    fn notify(&self) {
        if self.waiting.swap(false, Ordering::AcqRel) {
            (self.wake)();
        }
    }
}
fn byte_limit(bitrate: u32) -> usize {
    (u64::from(bitrate) * u64::from(BACKLOG_BUDGET_MS) / 8 / 1000).max(1) as usize
}
pub struct SendBudget {
    outstanding: Arc<AtomicUsize>,
    bitrate: Arc<AtomicU32>,
    limit: Arc<AtomicUsize>,
    wake: Option<Waiter>,
}
impl SendBudget {
    pub fn new(bitrate: u32) -> Self {
        Self {
            outstanding: Arc::default(),
            bitrate: Arc::new(AtomicU32::new(bitrate)),
            limit: Arc::new(AtomicUsize::new(byte_limit(bitrate))),
            wake: None,
        }
    }
    /// Called when queued frames free capacity: the budget drops back under its
    /// limit, or a frame finishes or is discarded. An encoder worker held by
    /// backpressure then wakes on the event instead of polling.
    pub fn with_wake(mut self, wake: Wake) -> Self {
        self.wake = Some(Waiter {
            waiting: Arc::default(),
            wake,
        });
        self
    }
    /// The worker is about to wait for capacity; the next release wakes it.
    pub fn wait_for_capacity(&self) {
        if let Some(waiter) = &self.wake {
            waiter.waiting.store(true, Ordering::Release);
        }
    }
    pub fn set_bitrate(&mut self, bitrate: u32) {
        self.bitrate.store(bitrate, Ordering::Release);
        self.limit.store(byte_limit(bitrate), Ordering::Release);
    }
    pub fn bytes(&self) -> usize {
        self.outstanding.load(Ordering::Acquire)
    }
    pub fn has_capacity(&self) -> bool {
        self.bytes() < self.limit.load(Ordering::Acquire)
    }
    pub fn track(&self, bytes: usize) -> PendingBytes {
        self.outstanding.fetch_add(bytes, Ordering::AcqRel);
        PendingBytes {
            outstanding: self.outstanding.clone(),
            bitrate: self.bitrate.clone(),
            limit: self.limit.clone(),
            wake: self.wake.clone(),
            remaining: bytes,
        }
    }
}

/// Ownership follows the queued/in-flight frame. Failed enqueue, recovery,
/// cancellation and receiver shutdown all release the remaining budget on drop.
pub struct PendingBytes {
    outstanding: Arc<AtomicUsize>,
    bitrate: Arc<AtomicU32>,
    limit: Arc<AtomicUsize>,
    wake: Option<Waiter>,
    remaining: usize,
}
impl PendingBytes {
    /// Follow this encoder's admission rate even for queued/in-flight frames.
    /// A fresh demand cut takes effect without waiting for the capture worker.
    pub fn pacing_bitrate(&self, demand: u32) -> u32 {
        self.bitrate
            .load(Ordering::Acquire)
            .min(transport_bitrate(demand))
    }

    pub fn sent(&mut self, bytes: usize) {
        assert!(bytes <= self.remaining);
        self.remaining -= bytes;
        let before = self.outstanding.fetch_sub(bytes, Ordering::AcqRel);
        let limit = self.limit.load(Ordering::Acquire);
        if before >= limit
            && before - bytes < limit
            && let Some(waiter) = &self.wake
        {
            waiter.notify();
        }
    }
}
impl Drop for PendingBytes {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(self.remaining, Ordering::AcqRel);
        if let Some(waiter) = &self.wake {
            waiter.notify();
        }
    }
}
/// Blocking sleeps for a dedicated pacing thread. On Windows this is a
/// high-resolution waitable timer (Windows 10 1803+), which wakes within about
/// a millisecond regardless of the process or system timer resolution, unlike
/// tokio's IOCP park (15.6 ms ticks unless the process raises its resolution,
/// which Windows 11 may also ignore while the window is minimized/occluded).
pub struct BlockingTimer {
    #[cfg(target_os = "windows")]
    handle: windows::Win32::Foundation::HANDLE,
}
// SAFETY: a kernel timer handle may be used from any thread. Concurrent sleeps
// would only re-arm each other's due time; the pacer owns one timer per thread.
#[cfg(target_os = "windows")]
unsafe impl Send for BlockingTimer {}
#[cfg(target_os = "windows")]
unsafe impl Sync for BlockingTimer {}
impl BlockingTimer {
    /// None when the OS lacks high-resolution timers; callers keep tokio pacing.
    pub fn new() -> Option<Self> {
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::System::Threading::*;
            // SAFETY: no attributes or name; the handle is closed on drop.
            let handle = unsafe {
                CreateWaitableTimerExW(
                    None,
                    windows::core::PCWSTR::null(),
                    CREATE_WAITABLE_TIMER_HIGH_RESOLUTION,
                    TIMER_ALL_ACCESS.0,
                )
            }
            .ok()?;
            Some(Self { handle })
        }
        #[cfg(not(target_os = "windows"))]
        {
            Some(Self {})
        }
    }
    pub fn sleep_until(&self, at: Instant) {
        let Some(wait) = at.checked_duration_since(Instant::now()) else {
            return;
        };
        #[cfg(target_os = "windows")]
        {
            use windows::Win32::System::Threading::*;
            // Negative due time is relative, in 100 ns units.
            let due = -i64::try_from(wait.as_nanos().div_ceil(100)).unwrap_or(i64::MAX);
            // SAFETY: valid owned handle and due-time pointer; no APC routine.
            let armed = unsafe { SetWaitableTimer(self.handle, &due, 0, None, None, false) };
            if armed.is_ok() {
                // SAFETY: waits on the owned timer handle only.
                unsafe { WaitForSingleObject(self.handle, INFINITE) };
                return;
            }
        }
        std::thread::sleep(wait);
    }
}
#[cfg(target_os = "windows")]
impl Drop for BlockingTimer {
    fn drop(&mut self) {
        // SAFETY: the handle was created by this value and is closed once.
        unsafe {
            let _ = windows::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

pub struct Pacer {
    next: Option<Instant>,
    bytes_per_second: u64,
    quantum: Duration,
    credit: Duration,
}
impl Pacer {
    /// Coarse async timers: 8 ms batch sleeps, one 16 ms OS tick of credit.
    pub fn new(bitrate: u32) -> Self {
        Self::with_timing(bitrate, Duration::from_millis(8), Duration::from_millis(16))
    }
    /// High-resolution timer: the same 16 ms burst credit (so frames after an
    /// idle gap are not delayed further), but 1 ms batches. Data beyond the
    /// credit leaves as an even trickle, and a wake can no longer overshoot by
    /// a 15.6 ms OS tick and release a second tick's worth of packets at once.
    pub fn fine(bitrate: u32) -> Self {
        Self::with_timing(bitrate, Duration::from_millis(1), Duration::from_millis(16))
    }
    fn with_timing(bitrate: u32, quantum: Duration, credit: Duration) -> Self {
        Self {
            next: None,
            bytes_per_second: u64::from(bitrate) / 8,
            quantum,
            credit,
        }
    }
    pub fn set_bitrate(&mut self, bitrate: u32, now: Instant) {
        let rate = (u64::from(bitrate) / 8).max(1);
        if rate == self.bytes_per_second {
            return;
        }
        // Preserve byte debt/credit through rate changes instead of granting a
        // fresh burst every time receiver feedback changes. reserve caps credit.
        let scale = self.bytes_per_second as f64 / rate as f64;
        self.next = self.next.map(|at| {
            if at >= now {
                now + at.duration_since(now).mul_f64(scale)
            } else {
                now - now
                    .duration_since(at)
                    .min(self.credit)
                    .mul_f64(scale)
                    .min(self.credit)
            }
        });
        self.bytes_per_second = rate;
    }
    pub fn reserve(&mut self, now: Instant, bytes: usize) -> Instant {
        // Keep at most one 16 ms OS timer tick of credit. An 8 ms sleep can
        // wake after 15.6 ms on Windows; capping credit at 8 ms discarded
        // half the byte budget every batch. Long stalls still cannot release
        // an unbounded catch-up burst.
        let quantum = self.quantum;
        let at = self.next.unwrap_or(now).max(now - self.credit);
        self.next =
            Some(at + Duration::from_secs_f64(bytes as f64 / self.bytes_per_second.max(1) as f64));
        if at <= now {
            now
        } else {
            // Sleep once for a packet batch, not once per sub-millisecond
            // packet interval. Keep byte debt anchored to the original clock.
            now + quantum
                * (at
                    .duration_since(now)
                    .as_nanos()
                    .div_ceil(quantum.as_nanos()) as u32)
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::{Outgoing, adaptation::QualityController};
    use thiscord_shared::screen::Quality;

    #[test]
    fn backlog_wakes_the_encoder_when_it_falls_under_budget_or_a_frame_ends() {
        let wakes = Arc::new(AtomicUsize::new(0));
        let counter = wakes.clone();
        // 8 Mbit/s for 100 ms is a 100 KB budget.
        let budget = SendBudget::new(8_000_000).with_wake(Arc::new(move || {
            counter.fetch_add(1, Ordering::Relaxed);
        }));
        let unwatched = budget.track(1);
        drop(unwatched);
        assert_eq!(wakes.load(Ordering::Relaxed), 0, "nobody was waiting");
        budget.wait_for_capacity();
        let mut frame = budget.track(150_000);
        assert!(!budget.has_capacity());
        frame.sent(40_000);
        assert_eq!(wakes.load(Ordering::Relaxed), 0, "still over budget");
        frame.sent(20_000);
        assert!(budget.has_capacity());
        assert_eq!(
            wakes.load(Ordering::Relaxed),
            1,
            "crossing under the limit wakes once"
        );
        frame.sent(20_000);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        budget.wait_for_capacity();
        drop(frame);
        assert_eq!(
            wakes.load(Ordering::Relaxed),
            2,
            "a finished frame frees queue space"
        );
        assert_eq!(budget.bytes(), 0);
    }

    // Real packetization, byte ownership/admission and pacing, with deterministic
    // capture and scheduler clocks. The first frame is a ~140 KB wire burst at
    // 1080p30; later deltas offer the encoder's nominal bitrate during the hold.
    fn replay_burst(quality: Quality, demand: u32, start: Instant) -> (Duration, usize) {
        let rate = transport_budget(quality.bitrate(), demand);
        let mut budget = SendBudget::new(transport_bitrate(quality.bitrate()));
        let mut pacer = Pacer::new(transport_bitrate(quality.bitrate()));
        let mut queue = std::collections::VecDeque::<Outgoing>::new();
        let mut sequence = 0;
        let mut next_capture = start;
        let mut skipped = 0;
        let mut now = start;
        let mut first_age = None;
        let mut captures = 0;
        let mut capture = |now: Instant,
                           queue: &mut std::collections::VecDeque<Outgoing>,
                           budget: &SendBudget| {
            while next_capture <= now && next_capture < start + Duration::from_millis(500) {
                if budget.has_capacity()
                    && crate::screen::encoder_has_capacity(
                        crate::screen::sender_queue_frames(quality.fps).saturating_sub(queue.len()),
                        0,
                    )
                {
                    let size = if captures == 0 {
                        (u64::from(quality.bitrate()) * 132_000 / 5_000_000) as usize
                    } else {
                        (quality.bitrate() / quality.fps / 8) as usize
                    };
                    let mut data = vec![1; size];
                    data[..5].copy_from_slice(&[0, 0, 0, 1, 0x61]);
                    let data = bytes::Bytes::from(data);
                    let packets =
                        crate::screen::packetize(data.clone(), &mut sequence, captures).unwrap();
                    let bytes = packets.iter().map(|p| p.payload.len() + 64).sum();
                    queue.push_back(Outgoing {
                        bitrate: quality.bitrate(),
                        captured_at: next_capture - Duration::from_millis(6),
                        enqueued_at: next_capture,
                        packets,
                        frame: data,
                        keyframe: captures == 0,
                        budget: budget.track(bytes),
                    });
                } else {
                    skipped += 1;
                }
                captures += 1;
                next_capture += Duration::from_secs_f64(1.0 / f64::from(quality.fps));
            }
            next_capture
        };
        loop {
            let next_capture = capture(now, &mut queue, &budget);
            // Change admission *after* tracking the first frame: in-flight
            // ownership must see the new rate, not a stale per-frame snapshot.
            budget.set_bitrate(rate);
            let Some(mut frame) = queue.pop_front() else {
                if next_capture >= start + Duration::from_millis(500) {
                    break;
                }
                now = next_capture;
                continue;
            };
            assert!(now.duration_since(frame.captured_at) <= crate::screen::SENDER_QUEUE_AGE);
            for packet in frame.packets {
                let bytes = packet.payload.len() + 64;
                assert_eq!(frame.budget.pacing_bitrate(demand), rate);
                pacer.set_bitrate(frame.budget.pacing_bitrate(demand), now);
                let at = pacer.reserve(now, bytes);
                if at > now {
                    now = at + Duration::from_millis(8); // Coarse 16 ms wake.
                }
                capture(now, &mut queue, &budget);
                frame.budget.sent(bytes);
            }
            let age = now.duration_since(frame.captured_at);
            assert!(
                age < Duration::from_millis(350),
                "{quality:?} / {demand}: {age:?}"
            );
            first_age.get_or_insert(age);
        }
        assert_eq!(budget.bytes(), 0);
        (first_age.unwrap(), skipped)
    }

    #[test]
    fn controller_transitions_and_bursts_share_demand_capped_admission_and_pacing() {
        let maximum = Quality::default();
        let mut controller = QualityController::new(maximum);
        let start = Instant::now();
        let mut baseline = Duration::ZERO;
        for (second, demand) in [
            (0, 5_000_000),
            (1, 4_900_000),
            (2, 5_000_000),
            (3, 4_900_000),
            (4, 5_000_000),
            (5, 4_900_000),
            (6, 4_900_000),
            (7, 4_900_000), // Sustained downgrade commits.
            (8, 5_000_000),
            (14, 5_000_000), // Recovery commits after dwell/headroom.
            (15, 200_000),   // Severe cut bypasses cooldown.
        ] {
            let now = start + Duration::from_secs(second);
            let quality = controller.update(demand, now).unwrap();
            let (age, skipped) = replay_burst(quality, demand, now);
            if second == 0 {
                baseline = age;
            }
            if second < 7 {
                assert_eq!(quality, maximum);
                assert!(
                    age <= baseline + Duration::from_millis(32),
                    "{age:?} vs {baseline:?}"
                );
                assert!(skipped > 0, "admission must shed raw input during bursts");
            } else if second < 14 {
                assert_eq!(quality, maximum.constrained(4_900_000));
            } else if second == 15 {
                assert_eq!(quality, maximum.constrained(demand));
            } else {
                assert_eq!(quality, maximum);
            }
        }
    }

    #[test]
    fn live_rate_cuts_bound_in_flight_traffic_without_erasing_pacer_debt() {
        let start = Instant::now();
        let maximum = Quality::default();
        let mut controller = QualityController::new(maximum);
        controller.update(5_000_000, start);
        let mut budget = SendBudget::new(transport_bitrate(maximum.bitrate()));
        let pending = budget.track(140_000);
        let mut pacer = Pacer::new(pending.pacing_bitrate(5_000_000));
        pacer.reserve(start, 75_000); // 100 ms of existing byte debt.
        // The sender sees fresh feedback before capture commits a new mode.
        assert_eq!(pending.pacing_bitrate(1_000_000), 1_200_000);
        pacer.set_bitrate(pending.pacing_bitrate(1_000_000), start);
        assert_eq!(
            pacer.reserve(start, 1_000),
            start + Duration::from_millis(504)
        );
        assert_eq!(pending.pacing_bitrate(0), 0); // Sender cancels, never paces zero.
        assert_eq!(controller.update(0, start), None);
        let resumed = controller.update(1_000_000, start).unwrap();
        budget.set_bitrate(transport_budget(resumed.bitrate(), 1_000_000));
        assert!(pending.pacing_bitrate(32_000_000) <= 1_200_000);
        assert!(!budget.has_capacity());
        drop(pending);
        assert!(budget.has_capacity());
    }

    #[test]
    fn reduced_rate_preserves_in_flight_byte_debt() {
        let mut budget = SendBudget::new(32_000_000);
        let mut pending = budget.track(20_000);
        assert!(budget.has_capacity());
        budget.set_bitrate(200_000);
        assert!(!budget.has_capacity());
        pending.sent(19_000);
        assert!(budget.has_capacity());
        drop(pending);
        assert_eq!(budget.bytes(), 0);
    }
    #[test]
    fn in_flight_bytes_block_encoding_and_release_on_send_or_cancel() {
        let budget = SendBudget::new(transport_bitrate(18_000_000));
        let mut frame = budget.track(555_000);
        assert!(!budget.has_capacity());
        frame.sent(300_001);
        assert!(budget.has_capacity());
        let queued = budget.track(40_000);
        assert!(!budget.has_capacity());
        drop(frame); // Partial write failure or cancellation.
        assert_eq!(budget.bytes(), 40_000);
        assert!(budget.has_capacity());
        drop(queued); // Queue teardown / discarded dependency.
        assert_eq!(budget.bytes(), 0);
    }

    #[test]
    fn large_frame_trace_avoids_stale_dependencies_without_faster_pacing() {
        // Replay repeated 700 KB wire bursts at
        // 60 fps. A coarse 9 ms wake models the reported Windows pacing sleeps.
        // Compare old slot-only admission with the same pacer plus byte admission.
        fn replay(byte_admission: bool) -> (usize, usize) {
            let start = Instant::now();
            let mut now = start;
            let mut next_capture = start;
            let mut captured = 0;
            let mut skipped = 0;
            let mut stale = 0;
            let mut queue = std::collections::VecDeque::new();
            let bitrate = transport_bitrate(18_000_000);
            let budget = SendBudget::new(bitrate);
            let mut pacer = Pacer::new(bitrate);
            // During each packet's pacing wait, new encoder outputs can arrive.
            let capture = |now: Instant,
                           queue: &mut std::collections::VecDeque<_>,
                           captured: &mut usize,
                           next: &mut Instant,
                           skipped: &mut usize| {
                while *next <= now && *captured < 300 {
                    if queue.len() < 12 && (!byte_admission || budget.has_capacity()) {
                        let bytes = if (*captured).is_multiple_of(60) {
                            700_000
                        } else {
                            35_000
                        };
                        queue.push_back((
                            *next - Duration::from_millis(6),
                            bytes,
                            budget.track(bytes),
                        ));
                    } else {
                        *skipped += 1;
                    }
                    *next += Duration::from_nanos(16_666_667);
                    *captured += 1;
                }
            };
            loop {
                capture(
                    now,
                    &mut queue,
                    &mut captured,
                    &mut next_capture,
                    &mut skipped,
                );
                let Some((at, mut remaining, mut frame)) = queue.pop_front() else {
                    if captured == 300 {
                        break;
                    }
                    now = next_capture;
                    continue;
                };
                if now.duration_since(at) > crate::screen::SENDER_QUEUE_AGE {
                    stale += 1;
                    continue;
                }
                while remaining > 0 {
                    let bytes = remaining.min(1164);
                    let deadline = pacer.reserve(now, bytes);
                    if deadline > now {
                        now = deadline + Duration::from_millis(1);
                    }
                    capture(
                        now,
                        &mut queue,
                        &mut captured,
                        &mut next_capture,
                        &mut skipped,
                    );
                    frame.sent(bytes);
                    remaining -= bytes;
                }
                assert!(now.duration_since(at) < Duration::from_millis(350));
            }
            assert_eq!(budget.bytes(), 0);
            (stale, skipped)
        }
        assert!(
            replay(false).0 > 0,
            "old admission must reproduce deadline misses"
        );
        let (stale, skipped) = replay(true);
        assert_eq!(
            stale, 0,
            "admitted H.264 dependencies must survive the burst"
        );
        assert!(skipped > 0, "shed raw input before creating dependencies");
    }
    #[test]
    fn stalls_do_not_release_a_catchup_burst() {
        let mut p = Pacer::new(8_000_000);
        let now = Instant::now();
        assert_eq!(p.reserve(now, 1000), now);
        assert_eq!(p.reserve(now, 1000), now + Duration::from_millis(8));
        let later = now + Duration::from_secs(1);
        assert_eq!(p.reserve(later, 1000), later);
        for _ in 0..16 {
            assert_eq!(p.reserve(later, 1000), later);
        }
        assert_eq!(p.reserve(later, 1000), later + Duration::from_millis(8));
    }
    #[test]
    fn batching_preserves_rate_with_coarse_wakeups() {
        let start = Instant::now();
        let mut now = start;
        let mut p = Pacer::new(8_000_000);
        let mut sleeps = 0;
        for _ in 0..1000 {
            let at = p.reserve(now, 1000);
            if at > now {
                sleeps += 1;
                now = at + Duration::from_millis(2);
            }
        }
        assert!(now.duration_since(start) >= Duration::from_millis(990));
        assert!(now.duration_since(start) <= Duration::from_millis(1010));
        assert!(
            sleeps < 150,
            "must batch instead of sleeping for every packet"
        );
    }
    #[test]
    fn windows_sixteen_ms_wakes_preserve_the_full_bitrate() {
        let start = Instant::now();
        let mut now = start;
        let mut pacer = Pacer::new(transport_bitrate(32_000_000));
        // A ready 4.8 MB stream must take one second even when each nominal
        // 8 ms sleep returns at the next 16 ms scheduler tick. The old 8 ms
        // credit ceiling discarded half the available transport capacity.
        for _ in 0..4000 {
            if pacer.reserve(now, 1200) > now {
                now += Duration::from_millis(16);
            }
        }
        assert!(
            (Duration::from_millis(990)..=Duration::from_millis(1024))
                .contains(&now.duration_since(start))
        );
    }
}
