use super::Assembler;
use bytes::Bytes;
use std::{
    collections::VecDeque,
    sync::{Arc, Condvar, Mutex},
    time::{Duration, Instant},
};
pub const MAX_AGE: Duration = Duration::from_millis(250);
const REORDER_AGE: Duration = Duration::from_millis(40);
const REORDER_PACKETS: usize = 256;
const QUEUE_FRAMES: usize = 8;
const QUEUE_BYTES: usize = 8 * 1024 * 1024;
// Per-packet and per-frame counters by compile-time slot (no name hashing).
use super::metrics::counter_index;
const RECEIVED_PACKETS: usize = counter_index("received_packets");
const RECEIVED_BYTES: usize = counter_index("received_bytes");
const LATE_OR_DUPLICATE: usize = counter_index("late_or_duplicate");
const REORDERED: usize = counter_index("reordered");
const REORDER_PEAK: usize = counter_index("reorder_peak_packets");
const RECEIVED_FRAMES: usize = counter_index("received_frames");
const PEAK_FRAMES: usize = counter_index("receive_peak_frames");
const PEAK_BYTES: usize = counter_index("receive_peak_bytes");
pub struct Frame {
    pub data: Bytes,
    pub arrived: Instant,
    pub epoch: u32,
    pub reset: bool,
    pub timestamp: u32,
}
/// Frames with a running byte total, so admission and the peak metric do
/// not sum the queue for every assembled frame.
#[derive(Default)]
struct Queue {
    frames: VecDeque<Frame>,
    bytes: usize,
}
impl Queue {
    fn len(&self) -> usize {
        self.frames.len()
    }
    fn bytes(&self) -> usize {
        self.bytes
    }
    fn clear(&mut self) {
        self.frames.clear();
        self.bytes = 0;
    }
    fn push_back(&mut self, frame: Frame) {
        self.bytes += frame.data.len();
        self.frames.push_back(frame);
        debug_assert_eq!(
            self.bytes,
            self.frames.iter().map(|f| f.data.len()).sum::<usize>()
        );
    }
    fn pop_front(&mut self) -> Option<Frame> {
        let frame = self.frames.pop_front()?;
        self.bytes -= frame.data.len();
        Some(frame)
    }
    /// Drops the `count` oldest frames.
    fn discard_front(&mut self, count: usize) {
        for frame in self.frames.drain(..count) {
            self.bytes -= frame.data.len();
        }
    }
    fn front(&self) -> Option<&Frame> {
        self.frames.front()
    }
    fn front_mut(&mut self) -> Option<&mut Frame> {
        self.frames.front_mut()
    }
    fn iter(&self) -> std::collections::vec_deque::Iter<'_, Frame> {
        self.frames.iter()
    }
    #[cfg(test)]
    fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}
#[derive(Default)]
struct State {
    assembler: Assembler,
    frames: Queue,
    sequence: Option<u16>,
    epoch: Option<u32>,
    timestamp: Option<u32>,
    arrived: Option<Instant>,
    synchronized: bool,
    closed: bool,
    reorder: Vec<(rtc::rtp::Packet, Instant)>,
    next: Option<u16>,
    feedback_at: Option<Instant>,
    requested: bool,
}
#[derive(Clone, Default)]
pub struct Inbox(Arc<Shared>);
#[derive(Default)]
struct Shared {
    state: Mutex<State>,
    ready: Condvar,
    metrics: super::metrics::Metrics,
}
pub struct Sender {
    inbox: Inbox,
}
impl Drop for Sender {
    fn drop(&mut self) {
        let mut s = self.inbox.0.state.lock().unwrap();
        s.closed = true;
        self.inbox.0.ready.notify_one();
    }
}
impl Sender {
    /// Returns [`Self::next_deadline`], computed under the same lock.
    pub fn push(&self, packet: rtc::rtp::Packet, arrived: Instant) -> Option<Instant> {
        let mut s = self.inbox.0.state.lock().unwrap();
        self.admit(&mut s, packet, arrived);
        Self::deadline(&s)
    }
    fn admit(&self, s: &mut State, packet: rtc::rtp::Packet, arrived: Instant) {
        if packet.payload.len() > 1500 {
            return;
        }
        let Some(epoch) = packet.header.csrc.first().copied() else {
            return;
        };
        if s.closed {
            return;
        }
        self.inbox.0.metrics.add_at(RECEIVED_PACKETS, 1);
        self.inbox
            .0
            .metrics
            .add_at(RECEIVED_BYTES, packet.payload.len() as u64);
        Self::switch_epoch(s, epoch);
        let next = *s.next.get_or_insert(packet.header.sequence_number);
        let delta = packet.header.sequence_number.wrapping_sub(next) as i16;
        if delta < 0
            || s.reorder
                .iter()
                .any(|(p, _)| p.header.sequence_number == packet.header.sequence_number)
        {
            self.inbox.0.metrics.add_at(LATE_OR_DUPLICATE, 1);
            return;
        }
        if delta > 0 {
            self.inbox.0.metrics.add_at(REORDERED, 1);
        }
        s.reorder.push((packet, arrived));
        self.inbox
            .0
            .metrics
            .peak_at(REORDER_PEAK, s.reorder.len() as u64);
        self.drain(s, arrived);
    }
    fn switch_epoch(s: &mut State, epoch: u32) {
        if s.epoch != Some(epoch) {
            s.frames.clear();
            s.assembler = Assembler::default();
            s.synchronized = false;
            s.timestamp = None;
            s.reorder.clear();
            s.next = None;
            s.feedback_at = None;
        }
        s.epoch = Some(epoch);
    }
    /// The own preview: a whole encoded frame from this client's sender. It
    /// skips reordering and reassembly (no packet clones, no copy) but keeps
    /// the same age, byte and keyframe-synchronization admission as received
    /// frames. Returns [`Self::next_deadline`].
    pub fn push_frame(
        &self,
        data: Bytes,
        epoch: u32,
        timestamp: u32,
        arrived: Instant,
    ) -> Option<Instant> {
        let mut s = self.inbox.0.state.lock().unwrap();
        if !s.closed && !data.is_empty() {
            self.inbox
                .0
                .metrics
                .add_at(RECEIVED_BYTES, data.len() as u64);
            Self::switch_epoch(&mut s, epoch);
            s.timestamp = Some(timestamp);
            s.arrived = Some(arrived);
            self.queue(&mut s, data, arrived, arrived, timestamp);
        }
        Self::deadline(&s)
    }
    pub fn tick(&self, now: Instant) {
        self.drain(&mut self.inbox.0.state.lock().unwrap(), now);
    }
    /// When `tick`/`keyframe_request` next have work: the oldest reordered
    /// packet's gap deadline, or the next allowed recovery request. `None`
    /// means idle (no queued gap, synchronized, nothing requested), so a video
    /// track without an active share needs no periodic timer at all.
    pub fn next_deadline(&self) -> Option<Instant> {
        Self::deadline(&self.inbox.0.state.lock().unwrap())
    }
    fn deadline(s: &State) -> Option<Instant> {
        if s.closed {
            return None;
        }
        let reorder = s.reorder.iter().map(|(_, at)| *at + REORDER_AGE).min();
        let feedback = (s.epoch.is_some() && (!s.synchronized || s.requested)).then(|| {
            s.feedback_at
                .map_or_else(Instant::now, |at| at + Duration::from_millis(500))
        });
        match (reorder, feedback) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
    /// Rate limited per stream; epoch prevents feedback targeting reused slots.
    pub fn keyframe_request(&self, now: Instant) -> Option<u32> {
        let mut s = self.inbox.0.state.lock().unwrap();
        if !s.closed
            && (!s.synchronized || s.requested)
            && s.feedback_at
                .is_none_or(|at| now.saturating_duration_since(at) >= Duration::from_millis(500))
        {
            let epoch = s.epoch?;
            s.feedback_at = Some(now);
            s.requested = false;
            self.inbox.0.metrics.add("keyframe_requests", 1);
            Some(epoch)
        } else {
            None
        }
    }
    fn drain(&self, s: &mut State, now: Instant) {
        while !s.reorder.is_empty() {
            let next = s.next.unwrap();
            let index = s
                .reorder
                .iter()
                .position(|(p, _)| p.header.sequence_number == next);
            let index = match index {
                Some(index) => index,
                None if s.reorder.len() >= REORDER_PACKETS
                    || s.reorder
                        .iter()
                        .any(|(_, at)| now.saturating_duration_since(*at) >= REORDER_AGE) =>
                {
                    let index = s
                        .reorder
                        .iter()
                        .enumerate()
                        .min_by_key(|(_, (p, _))| p.header.sequence_number.wrapping_sub(next))
                        .unwrap()
                        .0;
                    self.inbox.0.metrics.add(
                        "lost_packets",
                        u64::from(s.reorder[index].0.header.sequence_number.wrapping_sub(next)),
                    );
                    if s.synchronized {
                        self.inbox
                            .0
                            .metrics
                            .event("packet reorder deadline exceeded; requesting keyframe");
                    }
                    s.synchronized = false;
                    s.assembler = Assembler::default();
                    index
                }
                None => break,
            };
            let (packet, at) = s.reorder.swap_remove(index);
            s.next = Some(packet.header.sequence_number.wrapping_add(1));
            self.ordered(s, packet, at);
        }
    }
    fn ordered(&self, s: &mut State, packet: rtc::rtp::Packet, arrived: Instant) {
        s.sequence = Some(packet.header.sequence_number);
        if s.timestamp != Some(packet.header.timestamp) {
            s.timestamp = Some(packet.header.timestamp);
            s.arrived = Some(arrived);
        }
        let Some(data) = s.assembler.push(&packet) else {
            return;
        };
        let at = s.arrived.unwrap_or(arrived);
        self.queue(s, Bytes::from(data), at, arrived, packet.header.timestamp);
    }
    /// Admits one complete frame first seen at `at` (now `arrived`).
    fn queue(&self, s: &mut State, data: Bytes, at: Instant, arrived: Instant, timestamp: u32) {
        // Keep a decodable prefix across packet loss. On actual backlog,
        // prefer the newest queued keyframe and its dependent suffix.
        let recovery = super::recovery_frame(&data);
        if recovery && (s.frames.len() >= QUEUE_FRAMES || !s.synchronized) {
            s.frames.clear();
        }
        let over_budget = |s: &State| {
            s.frames.len() >= QUEUE_FRAMES
                || s.frames.bytes() + data.len() > QUEUE_BYTES
                || s.frames
                    .front()
                    .is_some_and(|f| arrived.saturating_duration_since(f.arrived) > MAX_AGE)
        };
        if over_budget(s) {
            if let Some(index) = s
                .frames
                .iter()
                .rposition(|f| super::recovery_frame(&f.data))
            {
                s.frames.discard_front(index);
                if let Some(f) = s.frames.front_mut() {
                    f.reset = true;
                }
            }
            if over_budget(s) {
                s.frames.clear();
                s.synchronized = false;
                self.inbox.0.metrics.add("queue_resets", 1);
                self.inbox
                    .0
                    .metrics
                    .event("receive backlog exceeded age/frame/byte limit");
            }
        }
        if arrived.saturating_duration_since(at) > MAX_AGE {
            s.synchronized = false;
            s.frames.clear();
            self.inbox.0.metrics.add("expired_frames", 1);
            return;
        }
        let reset = !s.synchronized;
        if reset && !recovery {
            return;
        }
        s.synchronized = true;
        s.frames.push_back(Frame {
            data,
            arrived: at,
            epoch: s.epoch.unwrap(),
            reset,
            timestamp,
        });
        self.inbox
            .0
            .metrics
            .peak_at(PEAK_FRAMES, s.frames.len() as u64);
        self.inbox
            .0
            .metrics
            .peak_at(PEAK_BYTES, s.frames.bytes() as u64);
        self.inbox.0.ready.notify_one();
        self.inbox.0.metrics.add_at(RECEIVED_FRAMES, 1);
        self.inbox
            .0
            .metrics
            .time("assembly", arrived.saturating_duration_since(at));
    }
}
impl Inbox {
    pub fn channel() -> (Sender, Self) {
        let inbox = Self::default();
        (
            Sender {
                inbox: inbox.clone(),
            },
            inbox,
        )
    }
    pub fn metrics(&self) -> super::metrics::Metrics {
        self.0.metrics.clone()
    }
    pub fn close(&self) {
        let mut s = self.0.state.lock().unwrap();
        s.closed = true;
        s.frames.clear();
        self.0.ready.notify_one();
    }
    pub fn request_keyframe(&self) {
        self.0.state.lock().unwrap().requested = true;
    }
    pub fn resync(&self) {
        let mut s = self.0.state.lock().unwrap();
        s.frames.clear();
        s.synchronized = false;
    }
    /// A new subscription may resume beyond RTP's half-range sequence window.
    /// Forget ordering and partial frames, but preserve closure and stream identity.
    pub fn restart_subscription(&self) {
        let mut s = self.0.state.lock().unwrap();
        *s = State {
            closed: s.closed,
            epoch: s.epoch,
            requested: true,
            ..Default::default()
        };
    }
    pub fn next(&self) -> Option<Frame> {
        let mut s = self.0.state.lock().unwrap();
        loop {
            if let Some(frame) = s.frames.pop_front() {
                if frame.arrived.elapsed() <= MAX_AGE {
                    return Some(frame);
                }
                self.0.metrics.add("expired_frames", 1);
                self.0
                    .metrics
                    .event("receive frame expired before playback");
                s.frames.clear();
                s.synchronized = false;
            }
            if s.closed {
                return None;
            }
            s = self.0.ready.wait(s).unwrap();
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn own_preview_frames_are_queued_whole_without_copies_and_wait_for_a_keyframe() {
        let (tx, rx) = Inbox::channel();
        let now = Instant::now();
        let delta = Bytes::from_static(&[0, 0, 0, 1, 0x41, 1, 2]);
        let idr = Bytes::from(vec![
            0, 0, 0, 1, 0x67, 66, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1, 2, 3,
        ]);
        // Unsynchronized: a delta frame is dropped and recovery is due now.
        assert!(tx.push_frame(delta.clone(), 3, 900, now).unwrap() <= Instant::now());
        assert_eq!(tx.keyframe_request(now), Some(3));
        tx.push_frame(idr.clone(), 3, 1800, now);
        tx.push_frame(delta.clone(), 3, 2700, now);
        assert_eq!(
            rx.metrics().snapshot().counters["received_frames"],
            2,
            "both queued"
        );
        let first = rx.next().unwrap();
        assert!(first.reset);
        assert_eq!(first.timestamp, 1800);
        // The queued frame is the sender's buffer, not a reassembled copy.
        assert_eq!(first.data.as_ptr(), idr.as_ptr());
        let second = rx.next().unwrap();
        assert!(!second.reset);
        assert_eq!(second.data.as_ptr(), delta.as_ptr());
        // A new source epoch drops the old stream state like received packets do.
        tx.push_frame(delta, 4, 3600, now);
        assert_eq!(tx.keyframe_request(now + Duration::from_secs(1)), Some(4));
        let metrics = rx.metrics().snapshot();
        assert_eq!(metrics.counters["received_frames"], 2);
        assert_eq!(
            metrics
                .counters
                .get("received_packets")
                .copied()
                .unwrap_or(0),
            0
        );
    }
    #[test]
    fn idle_tracks_have_no_deadline_and_gaps_or_recovery_schedule_one() {
        let (tx, _rx) = Inbox::channel();
        // A negotiated slot nobody is sharing on: no timer needed.
        assert_eq!(tx.next_deadline(), None);
        let now = Instant::now();
        let mut seq = 10;
        let mut packets = crate::screen::packetize(vec![0, 0, 0, 1, 0x65, 1, 2, 3], &mut seq, 9000)
            .unwrap()
            .into_iter()
            .chain(crate::screen::packetize(vec![0, 0, 0, 1, 0x41, 1], &mut seq, 18000).unwrap())
            .chain(crate::screen::packetize(vec![0, 0, 0, 1, 0x41, 2], &mut seq, 27000).unwrap())
            .map(|mut p| {
                p.header.csrc = vec![7];
                p
            })
            .collect::<Vec<_>>();
        // Unsynchronized (no IDR yet): recovery may be requested immediately.
        tx.push(packets.remove(0), now);
        assert!(tx.next_deadline().unwrap() <= Instant::now());
        assert_eq!(tx.keyframe_request(now), Some(7));
        let next = tx.next_deadline().unwrap();
        assert!(next >= now + Duration::from_millis(500));
        // A sequence gap schedules its reorder deadline before that.
        tx.push(packets.remove(1), now);
        assert_eq!(tx.next_deadline(), Some(now + REORDER_AGE));
        tx.tick(now + REORDER_AGE);
        assert_eq!(tx.next_deadline(), Some(next));
    }
    fn frame(sender: &Sender, seq: &mut u16, key: bool, at: Instant) {
        let data = if key {
            vec![0, 0, 1, 0x67, 66, 0, 0, 1, 0x68, 1, 0, 0, 1, 0x65, 1]
        } else {
            vec![0, 0, 1, 0x61, 1]
        };
        for mut packet in crate::screen::packetize(data, seq, *seq as u32 * 9000).unwrap() {
            packet.header.csrc = vec![1];
            sender.push(packet, at);
        }
    }
    #[test]
    fn restored_subscription_accepts_large_sequence_gap_and_requires_keyframe() {
        let (tx, rx) = Inbox::channel();
        let mut seq = 0;
        frame(&tx, &mut seq, true, Instant::now());
        assert!(rx.next().unwrap().reset);
        seq = seq.wrapping_add(40_000);
        rx.restart_subscription();
        assert_eq!(tx.keyframe_request(Instant::now()), Some(1));
        frame(&tx, &mut seq, false, Instant::now());
        assert!(rx.0.state.lock().unwrap().frames.is_empty());
        frame(&tx, &mut seq, true, Instant::now());
        assert_eq!(rx.0.state.lock().unwrap().frames.len(), 1);
        assert!(rx.next().unwrap().reset);
        rx.close();
        rx.restart_subscription();
        frame(&tx, &mut seq, true, Instant::now());
        assert!(
            rx.next().is_none(),
            "restart must not reopen a closed inbox"
        );
    }
    #[test]
    fn backlog_discards_dependencies_until_fresh_keyframe() {
        let (tx, rx) = Inbox::channel();
        let mut seq = 0;
        frame(&tx, &mut seq, true, Instant::now());
        frame(&tx, &mut seq, false, Instant::now());
        frame(&tx, &mut seq, false, Instant::now());
        assert_eq!(
            rx.0.state.lock().unwrap().frames.len(),
            3,
            "short backlog must drain without a freeze"
        );
        for _ in 0..QUEUE_FRAMES - 2 {
            frame(&tx, &mut seq, false, Instant::now());
        }
        assert!(rx.0.state.lock().unwrap().frames.is_empty());
        frame(&tx, &mut seq, false, Instant::now());
        assert!(rx.0.state.lock().unwrap().frames.is_empty());
        frame(&tx, &mut seq, true, Instant::now());
        assert!(rx.next().unwrap().reset);
        seq += 1; // Packet loss invalidates inter-frame references.
        frame(&tx, &mut seq, false, Instant::now());
        tx.tick(Instant::now() + REORDER_AGE);
        assert!(rx.0.state.lock().unwrap().frames.is_empty());
        frame(&tx, &mut seq, true, Instant::now());
        assert!(rx.next().unwrap().reset);
    }
    #[test]
    fn reorder_wrap_duplicates_and_feedback_deadline() {
        let (tx, rx) = Inbox::channel();
        let now = Instant::now();
        let mut seq = u16::MAX - 3;
        frame(&tx, &mut seq, true, now);
        assert!(rx.next().unwrap().reset);
        let mut data = vec![0, 0, 1, 0x61];
        data.extend(vec![42; 4000]);
        let mut packets = crate::screen::packetize(data.clone(), &mut seq, 90_000).unwrap();
        for p in &mut packets {
            p.header.csrc = vec![1];
        }
        tx.push(packets[1].clone(), now);
        tx.push(packets[1].clone(), now);
        tx.push(packets[0].clone(), now + Duration::from_millis(10));
        for p in packets.into_iter().skip(2) {
            tx.push(p, now + Duration::from_millis(15));
        }
        assert_eq!(
            rx.next().unwrap().data,
            [vec![0, 0, 0, 1, 0x61], vec![42; 4000]].concat()
        );
        assert!(tx.keyframe_request(now).is_none());
        seq = seq.wrapping_add(1);
        frame(&tx, &mut seq, false, now);
        tx.tick(now + Duration::from_millis(39));
        assert!(tx.keyframe_request(now).is_none());
        tx.tick(now + Duration::from_millis(41));
        assert_eq!(
            tx.keyframe_request(now + Duration::from_millis(41)),
            Some(1)
        );
        assert!(
            tx.keyframe_request(now + Duration::from_millis(42))
                .is_none()
        );
        assert_eq!(
            tx.keyframe_request(now + Duration::from_millis(550)),
            Some(1)
        );
        frame(&tx, &mut seq, true, now);
        assert!(rx.next().unwrap().reset);
        assert!(tx.keyframe_request(now + Duration::from_secs(1)).is_none());
    }
    #[test]
    fn overflow_keeps_a_newer_recovery_point_and_its_suffix() {
        let (tx, rx) = Inbox::channel();
        let mut seq = 0;
        let now = Instant::now();
        frame(&tx, &mut seq, true, now);
        for _ in 0..3 {
            frame(&tx, &mut seq, false, now);
        }
        frame(&tx, &mut seq, true, now);
        for _ in 0..4 {
            frame(&tx, &mut seq, false, now);
        }
        assert_eq!(rx.0.state.lock().unwrap().frames.len(), 5);
        assert!(rx.next().unwrap().reset);
        assert!(tx.keyframe_request(now).is_none());
    }
    #[test]
    fn age_is_measured_at_arrival_not_after_processing() {
        let (tx, rx) = Inbox::channel();
        let mut seq = 0;
        frame(&tx, &mut seq, true, Instant::now() - Duration::from_secs(1));
        drop(tx);
        assert!(rx.next().is_none());
        assert!(!rx.0.state.lock().unwrap().synchronized);
    }
}
