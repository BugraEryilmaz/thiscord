use super::Assembler;
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
pub struct Frame {
    pub data: Vec<u8>,
    pub arrived: Instant,
    pub epoch: u32,
    pub reset: bool,
    pub timestamp: u32,
}
#[derive(Default)]
struct State {
    assembler: Assembler,
    frames: VecDeque<Frame>,
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
    pub fn push(&self, packet: rtc::rtp::Packet, arrived: Instant) {
        if packet.payload.len() > 1500 {
            return;
        }
        let Some(epoch) = packet.header.csrc.first().copied() else {
            return;
        };
        let mut s = self.inbox.0.state.lock().unwrap();
        if s.closed {
            return;
        }
        self.inbox.0.metrics.add("received_packets", 1);
        self.inbox
            .0
            .metrics
            .add("received_bytes", packet.payload.len() as u64);
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
        let next = *s.next.get_or_insert(packet.header.sequence_number);
        let delta = packet.header.sequence_number.wrapping_sub(next) as i16;
        if delta < 0
            || s.reorder
                .iter()
                .any(|(p, _)| p.header.sequence_number == packet.header.sequence_number)
        {
            self.inbox.0.metrics.add("late_or_duplicate", 1);
            return;
        }
        if delta > 0 {
            self.inbox.0.metrics.add("reordered", 1);
        }
        s.reorder.push((packet, arrived));
        self.inbox
            .0
            .metrics
            .peak("reorder_peak_packets", s.reorder.len() as u64);
        self.drain(&mut s, arrived);
    }
    pub fn tick(&self, now: Instant) {
        self.drain(&mut self.inbox.0.state.lock().unwrap(), now);
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
        // Keep a decodable prefix across packet loss. On actual backlog,
        // prefer the newest queued keyframe and its dependent suffix.
        let recovery = super::recovery_frame(&data);
        if recovery && (s.frames.len() >= QUEUE_FRAMES || !s.synchronized) {
            s.frames.clear();
        }
        let over_budget = |s: &State| {
            s.frames.len() >= QUEUE_FRAMES
                || s.frames.iter().map(|f| f.data.len()).sum::<usize>() + data.len() > QUEUE_BYTES
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
                s.frames.drain(..index);
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
            timestamp: packet.header.timestamp,
        });
        self.inbox
            .0
            .metrics
            .peak("receive_peak_frames", s.frames.len() as u64);
        self.inbox.0.metrics.peak(
            "receive_peak_bytes",
            s.frames.iter().map(|f| f.data.len() as u64).sum(),
        );
        self.inbox.0.ready.notify_one();
        self.inbox.0.metrics.add("received_frames", 1);
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
