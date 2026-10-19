//! Queued work from a closed peer cannot revive capture or reach a replacement.
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicBool, AtomicU64, Ordering},
};
use thiscord_shared::RequestId;
static NEXT_ID: AtomicU64 = AtomicU64::new(1);
#[derive(Clone)]
pub struct Connection {
    alive: Arc<AtomicBool>,
    diagnostic_id: u64,
    request_id: Arc<OnceLock<RequestId>>,
    pub metrics: Arc<super::quality::Metrics>,
    uplink: Arc<super::adaptive_fec::Feedback>,
}
impl Default for Connection {
    fn default() -> Self {
        Self {
            alive: Arc::new(AtomicBool::new(true)),
            diagnostic_id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            request_id: Arc::default(),
            metrics: Arc::default(),
            uplink: Arc::default(),
        }
    }
}
impl Connection {
    pub(super) fn observe_uplink(
        &self,
        report: &rtc::rtcp::reception_report::ReceptionReport,
        now: std::time::Instant,
    ) {
        if self.active() {
            self.uplink.observe(self.diagnostic_id, report, now);
        }
    }
    /// Source generation is stable across PTT/privacy revisions; use a new value
    /// only for an authoritative microphone source replacement. Peer replacement
    /// already creates a fresh Connection. Current calls have one source (0).
    pub fn uplink_sent(&self, generation: u64, sequence: u16) {
        if self.active() {
            self.uplink.sent(generation, sequence);
        }
    }
    pub(crate) fn uplink_sample(&self, generation: u64) -> Option<super::adaptive_fec::Sample> {
        self.active()
            .then(|| self.uplink.sample(generation))
            .flatten()
    }
    pub fn request_id(&self) -> Option<RequestId> {
        self.request_id.get().copied()
    }
    pub fn set_request_id(&self, id: RequestId) {
        let _ = self.request_id.set(id);
    }
    pub fn diagnostic_details(&self, mut details: serde_json::Value) -> serde_json::Value {
        details["connection_id"] = self.diagnostic_id.into();
        details["request_id"] = serde_json::json!(self.request_id.get());
        details
    }
    pub fn event(&self, name: &'static str, details: serde_json::Value) {
        super::diagnostics::event(name, self.diagnostic_details(details));
    }
    pub fn close(&self) {
        self.alive.store(false, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        self.alive.load(Ordering::Acquire)
    }
    pub fn accepts(&self, other: &Self) -> bool {
        self.active() && other.active() && Arc::ptr_eq(&self.alive, &other.alive)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retry_has_a_new_identity_and_close_invalidates_all_queued_clones() {
        let old = Connection::default();
        let queued = old.clone();
        let retry = Connection::default();
        assert!(old.accepts(&queued));
        assert!(!retry.accepts(&queued));
        old.close();
        assert!(!queued.active());
        assert!(!old.accepts(&queued));
        assert!(retry.active());
    }
    #[test]
    fn request_id_reaches_existing_clones_but_not_the_next_attempt() {
        let connection = Connection::default();
        let audio_worker = connection.clone();
        let retry = Connection::default();
        let id = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        connection.set_request_id(id);
        let fields = audio_worker.diagnostic_details(serde_json::json!({}));
        assert_eq!(fields["request_id"], id.to_string());
        let next = retry.diagnostic_details(serde_json::json!({}));
        assert_ne!(fields["connection_id"], next["connection_id"]);
        assert!(next["request_id"].is_null());
    }
}
