//! Queued work from a closed peer cannot revive capture or reach a replacement.
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
#[derive(Clone)]
pub struct Connection(Arc<AtomicBool>);
impl Default for Connection {
    fn default() -> Self {
        Self(Arc::new(AtomicBool::new(true)))
    }
}
impl Connection {
    pub fn close(&self) {
        self.0.store(false, Ordering::Release);
    }
    pub fn active(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub fn accepts(&self, other: &Self) -> bool {
        self.active() && other.active() && Arc::ptr_eq(&self.0, &other.0)
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
}
