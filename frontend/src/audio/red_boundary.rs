//! One bounded authenticated boundary exchange per voice connection. Superseded
//! acknowledgments cannot admit audio; rapid local changes coalesce while waiting.
use crate::audio::{connection::Connection, outgoing::EncodedFrame};
use std::time::{Duration, Instant};

#[derive(Default)]
pub struct Boundary {
    control: Option<u64>,
    delivery: Option<Connection>,
    tag: u64,
    pending: Option<(u64, Instant, Option<u32>)>,
    challenge: Option<u32>,
    required: u32,
    satisfied: u32,
    rejected: u32,
    rejection_started: Option<Instant>,
    ready_notified: Option<(u64, u32)>,
    acknowledged: u64,
}
impl Boundary {
    fn advance(&mut self) -> Result<(), &'static str> {
        self.tag = self
            .tag
            .checked_add(1)
            .ok_or("Microphone generation exhausted; rejoin voice")?;
        Ok(())
    }
    pub fn control(&mut self, generation: u64) -> Result<bool, &'static str> {
        if self.control == Some(generation) {
            return Ok(false);
        }
        self.advance()?;
        self.control = Some(generation);
        self.delivery = None;
        self.challenge = None;
        Ok(true)
    }
    pub fn frame(&mut self, frame: &EncodedFrame) -> Result<bool, &'static str> {
        let mut changed = self.control(frame.generation)?;
        if self
            .delivery
            .as_ref()
            .is_some_and(|d| !d.accepts(&frame.delivery))
        {
            self.advance()?;
            changed = true;
        }
        self.delivery = Some(frame.delivery.clone());
        Ok(changed)
    }
    pub fn require_refresh(&mut self, challenge: u32) {
        self.required = self.required.max(challenge);
    }
    pub fn needs_refresh(&self) -> bool {
        self.pending.is_none()
            && self.required > self.satisfied
            && self.required > self.rejected
            && (self.ready() || self.challenge.is_some())
    }
    pub fn start_refresh(&mut self, control: u64) -> Result<(), &'static str> {
        self.control(control)?;
        self.challenge = Some(self.required);
        Ok(())
    }
    pub fn request(&mut self, now: Instant) -> Result<Option<(u64, Option<u32>)>, &'static str> {
        if self
            .rejection_started
            .is_some_and(|at| now.saturating_duration_since(at) >= Duration::from_secs(5))
        {
            return Err("Microphone refresh was rejected; rejoin voice");
        }
        if self
            .pending
            .is_some_and(|(_, at, _)| now.saturating_duration_since(at) >= Duration::from_secs(5))
        {
            return Err("Microphone boundary acknowledgment timed out");
        }
        if self.pending.is_none()
            && self.tag != 0
            && self.acknowledged != self.tag
            && self.challenge.is_none_or(|c| c > self.rejected)
        {
            self.pending = Some((self.tag, now, self.challenge));
            return Ok(Some((self.tag, self.challenge)));
        }
        Ok(None)
    }
    pub fn acknowledge(&mut self, tag: u64, challenge: Option<u32>, accepted: bool) -> bool {
        if self
            .pending
            .is_some_and(|(pending, _, kind)| pending == tag && kind == challenge)
        {
            self.pending = None;
            if !accepted && let Some(challenge) = challenge {
                self.rejected = self.rejected.max(challenge);
                self.rejection_started.get_or_insert_with(Instant::now);
            }
            if accepted && let Some(challenge) = challenge {
                self.satisfied = self.satisfied.max(challenge);
            }
            if accepted && tag == self.tag && challenge.is_none_or(|c| c >= self.required) {
                if challenge.is_some() {
                    self.rejection_started = None;
                }
                self.acknowledged = tag;
                return true;
            }
        }
        false
    }
    pub fn resume(&mut self) {
        self.pending = None;
        self.acknowledged = 0;
        self.ready_notified = None;
    }
    pub fn ready_control(&self, control: u64) -> bool {
        self.control == Some(control) && self.ready()
    }
    pub fn ready(&self) -> bool {
        self.tag != 0 && self.acknowledged == self.tag
    }
    /// Causal proof that this client processed the ACK, even if signaling batched
    /// its delivery. The SFU starts the next-challenge cooldown from this notice.
    pub fn ready_notice(&mut self) -> Option<(u64, u32)> {
        let notice = (self.tag, self.satisfied);
        if !self.ready() || self.satisfied == 0 || self.ready_notified == Some(notice) {
            return None;
        }
        self.ready_notified = Some(notice);
        Some(notice)
    }
    pub fn csrc(&self) -> Vec<u32> {
        vec![(self.tag >> 32) as u32, self.tag as u32]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rapid_boundaries_coalesce_and_old_ack_cannot_release_audio() {
        let now = Instant::now();
        let mut b = Boundary::default();
        b.control(0).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((1, None)));
        for control in 1..=100 {
            b.control(control).unwrap();
            assert_eq!(b.request(now).unwrap(), None);
        }
        b.acknowledge(1, None, true);
        assert!(!b.ready());
        assert_eq!(b.request(now).unwrap(), Some((101, None)));
        b.acknowledge(100, None, true);
        assert!(!b.ready());
        b.acknowledge(101, None, true);
        assert!(b.ready());
        b.resume();
        assert!(!b.ready());
        assert_eq!(b.request(now).unwrap(), Some((101, None)));
        b.acknowledge(101, None, true);
        assert!(b.ready());
        let mut other_connection = Boundary::default();
        other_connection.acknowledge(101, None, true);
        assert!(!other_connection.ready());
    }
    #[test]
    fn refresh_challenges_coalesce_without_turning_into_receive_boundaries() {
        let now = Instant::now();
        let mut b = Boundary::default();
        b.control(1).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((1, None)));
        b.require_refresh(7);
        assert!(
            !b.needs_refresh(),
            "finish the outstanding local boundary first"
        );
        assert!(b.acknowledge(1, None, true));
        assert!(b.needs_refresh());
        b.start_refresh(2).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((2, Some(7))));
        b.require_refresh(8);
        b.require_refresh(6);
        assert!(!b.needs_refresh());
        assert!(!b.acknowledge(2, Some(8), true), "wrong challenge ACK");
        assert!(!b.acknowledge(2, Some(7), false));
        assert!(!b.ready());
        assert!(b.needs_refresh());
        b.start_refresh(3).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((3, Some(8))));
        assert!(b.acknowledge(3, Some(8), true));
        assert!(!b.needs_refresh());
        // An actual local PTT/settings transition still takes the full path.
        b.control(4).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((4, None)));
        assert!(!b.acknowledge(3, Some(8), true));
        assert!(b.acknowledge(4, None, true));
        b.require_refresh(9);
        b.start_refresh(5).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((5, Some(9))));
        b.require_refresh(10);
        assert!(
            !b.acknowledge(5, Some(9), true),
            "accepted superseded challenge cannot resume"
        );
        assert!(!b.ready());
        assert!(b.needs_refresh());
        // A genuine local boundary outranks the next source-only refresh.
        b.control(6).unwrap();
        assert!(!b.needs_refresh());
        assert_eq!(b.request(now).unwrap(), Some((6, None)));
        assert!(b.acknowledge(6, None, true));
        assert!(b.needs_refresh());
    }
    #[test]
    fn lost_proof_ack_then_local_boundary_reproves_before_ready_after_resume() {
        let now = Instant::now();
        let mut b = Boundary::default();
        b.control(1).unwrap();
        b.request(now).unwrap();
        b.acknowledge(1, None, true);
        b.require_refresh(7);
        b.start_refresh(2).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((2, Some(7))));
        // SFU accepted g2/c7, but its ACK was lost. A real local boundary wins.
        b.control(3).unwrap();
        b.resume();
        assert_eq!(b.request(now).unwrap(), Some((3, None)));
        assert!(b.acknowledge(3, None, true));
        assert_eq!(b.ready_notice(), None);
        assert!(b.needs_refresh());
        b.start_refresh(4).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((4, Some(7))));
        assert!(!b.acknowledge(2, Some(7), true));
        assert!(b.acknowledge(4, Some(7), true));
        assert_eq!(b.ready_notice(), Some((4, 7)));
        assert_eq!(b.ready_notice(), None);
        assert!(b.request(now + Duration::from_secs(6)).is_ok());
    }
    #[test]
    fn ready_notice_requires_current_processed_ack_and_never_repeats() {
        let now = Instant::now();
        let mut b = Boundary::default();
        b.control(1).unwrap();
        b.request(now).unwrap();
        b.acknowledge(1, None, true);
        assert_eq!(b.ready_notice(), None);
        b.require_refresh(7);
        b.start_refresh(2).unwrap();
        b.request(now).unwrap();
        assert_eq!(b.ready_notice(), None, "ACK still held in the network");
        b.control(3).unwrap(); // genuine local boundary has priority
        assert!(!b.acknowledge(2, Some(7), true));
        assert_eq!(b.ready_notice(), None);
        b.request(now).unwrap();
        assert!(b.acknowledge(3, None, true));
        assert_eq!(b.ready_notice(), Some((3, 7)));
        assert_eq!(b.ready_notice(), None);
    }
    #[test]
    fn rejected_refresh_waits_for_new_challenge_with_a_bounded_deadline() {
        let now = Instant::now();
        let mut b = Boundary::default();
        b.control(1).unwrap();
        b.request(now).unwrap();
        assert!(b.acknowledge(1, None, true));
        b.require_refresh(7);
        b.start_refresh(2).unwrap();
        b.request(now).unwrap();
        assert!(!b.acknowledge(2, Some(7), false));
        for _ in 0..100 {
            assert!(!b.needs_refresh());
            assert_eq!(b.request(now).unwrap(), None);
        }
        assert!(b.request(now + Duration::from_secs(6)).is_err());
        b.require_refresh(8);
        assert!(b.needs_refresh());
        b.start_refresh(3).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((3, Some(8))));
        assert!(b.acknowledge(3, Some(8), true));
        assert!(b.request(now + Duration::from_secs(6)).is_ok());
    }
    #[test]
    fn signaling_recovery_retries_without_releasing_superseded_control() {
        let now = Instant::now();
        let mut b = Boundary::default();
        b.control(1).unwrap();
        assert_eq!(b.request(now).unwrap(), Some((1, None)));
        // Recovery clears the attached-socket deadline, while keeping audio held.
        b.resume();
        b.control(2).unwrap();
        assert!(!b.acknowledge(1, None, true));
        assert_eq!(
            b.request(now + Duration::from_secs(19)).unwrap(),
            Some((2, None))
        );
        assert!(b.acknowledge(2, None, true));
        assert!(b.ready_control(2));
        assert!(!b.ready_control(3));
    }
    #[test]
    fn ack_timeout_and_tag_exhaustion_fail_closed() {
        let now = Instant::now();
        let mut b = Boundary::default();
        b.control(7).unwrap();
        b.request(now).unwrap();
        assert!(b.request(now + Duration::from_secs(5)).is_err());
        b.tag = u64::MAX;
        assert!(b.control(8).is_err());
        assert_eq!(b.tag, u64::MAX);
        assert!(!b.ready());
    }
}
