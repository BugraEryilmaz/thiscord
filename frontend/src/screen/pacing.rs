//! Byte pacing with a small bounded burst and no catch-up after a scheduler stall.
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

pub fn transport_bitrate(codec_bitrate: u32) -> u32 {
    codec_bitrate * 12 / 10
}

/// Count unsent wire bytes, including the frame currently being paced. Frame
/// count alone cannot distinguish a small delta from a 500 KB scene change.
/// This is raw-input admission, not an encoded-frame size/drop limit.
pub const BACKLOG_BUDGET_MS: u32 = 50;
pub struct SendBudget {
    outstanding: Arc<AtomicUsize>,
    limit: usize,
}
impl SendBudget {
    pub fn new(bitrate: u32) -> Self {
        Self {
            outstanding: Arc::default(),
            limit: (u64::from(bitrate) * u64::from(BACKLOG_BUDGET_MS) / 8 / 1000).max(1) as usize,
        }
    }
    pub fn bytes(&self) -> usize {
        self.outstanding.load(Ordering::Acquire)
    }
    pub fn has_capacity(&self) -> bool {
        self.bytes() < self.limit
    }
    pub fn track(&self, bytes: usize) -> PendingBytes {
        self.outstanding.fetch_add(bytes, Ordering::AcqRel);
        PendingBytes {
            outstanding: self.outstanding.clone(),
            remaining: bytes,
        }
    }
}

/// Ownership follows the queued/in-flight frame. Failed enqueue, recovery,
/// cancellation and receiver shutdown all release the remaining budget on drop.
pub struct PendingBytes {
    outstanding: Arc<AtomicUsize>,
    remaining: usize,
}
impl PendingBytes {
    pub fn sent(&mut self, bytes: usize) {
        assert!(bytes <= self.remaining);
        self.remaining -= bytes;
        self.outstanding.fetch_sub(bytes, Ordering::AcqRel);
    }
}
impl Drop for PendingBytes {
    fn drop(&mut self) {
        self.outstanding.fetch_sub(self.remaining, Ordering::AcqRel);
    }
}
pub struct Pacer {
    next: Option<Instant>,
    bytes_per_second: u64,
}
impl Pacer {
    pub fn new(bitrate: u32) -> Self {
        Self {
            next: None,
            bytes_per_second: u64::from(bitrate) / 8,
        }
    }
    pub fn reserve(&mut self, now: Instant, bytes: usize) -> Instant {
        // At most 8 ms of burst credit amortizes OS timer resolution. A stall
        // never accumulates an entire frame/second of catch-up credit.
        let quantum = Duration::from_millis(8);
        let at = self.next.unwrap_or(now).max(now - quantum);
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
    #[test]
    fn in_flight_bytes_block_encoding_and_release_on_send_or_cancel() {
        let budget = SendBudget::new(transport_bitrate(18_000_000));
        let mut frame = budget.track(555_000);
        assert!(!budget.has_capacity());
        frame.sent(420_001);
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
        // Replay repeated 524 KB access units (~555 KB with RTP overhead) at
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
                            555_000
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
        for _ in 0..8 {
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
}
