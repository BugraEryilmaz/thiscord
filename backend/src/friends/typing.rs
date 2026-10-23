//! Socket-owned typing leases. Disconnect/switch removes only that device.
use std::{
    collections::HashMap,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use thiscord_shared::{AccountId, ids::ConversationId};
use uuid::Uuid;
type Lease = (ConversationId, AccountId, Instant);
fn leases() -> &'static Mutex<HashMap<Uuid, Lease>> {
    static LEASES: OnceLock<Mutex<HashMap<Uuid, Lease>>> = OnceLock::new();
    LEASES.get_or_init(Default::default)
}
pub(super) struct LeaseOwner(pub Uuid);
impl LeaseOwner {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
    pub fn clear(&self) {
        leases().lock().unwrap().remove(&self.0);
    }
    pub fn set(&self, conversation: ConversationId, actor: AccountId, active: bool) {
        let mut leases = leases().lock().unwrap();
        if active {
            leases.insert(
                self.0,
                (conversation, actor, Instant::now() + Duration::from_secs(4)),
            );
        } else {
            leases.remove(&self.0);
        }
    }
}
impl Drop for LeaseOwner {
    fn drop(&mut self) {
        self.clear();
    }
}
pub(super) fn read(conversation: ConversationId, actor: AccountId) -> Vec<AccountId> {
    let mut leases = leases().lock().unwrap();
    leases.retain(|_, (_, _, until)| *until > Instant::now());
    let mut people: Vec<_> = leases
        .values()
        .filter(|(id, a, _)| *id == conversation && *a != actor)
        .map(|(_, a, _)| *a)
        .collect();
    people.sort();
    people.dedup();
    people
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_leases_expire_and_disconnect_independently() {
        let c = ConversationId::from_uuid(Uuid::new_v4());
        let a = AccountId::from_uuid(Uuid::new_v4());
        let b = AccountId::from_uuid(Uuid::new_v4());
        let first = LeaseOwner::new();
        let second = LeaseOwner::new();
        first.set(c, a, true);
        second.set(c, a, true);
        assert_eq!(read(c, b), vec![a]);
        drop(first);
        assert_eq!(read(c, b), vec![a]);
        second.clear();
        assert!(read(c, b).is_empty());
        second.set(c, a, true);
        leases().lock().unwrap().get_mut(&second.0).unwrap().2 =
            Instant::now() - Duration::from_secs(1);
        assert!(read(c, b).is_empty());
    }
}
