//! Notification presentation checks and bounded FIFO for account preference writes.
//! Revisions are assigned only when
//! a command leaves the queue, after the preceding server response was applied.
use std::collections::VecDeque;
use thiscord_shared::{
    ids::CallId,
    notifications::{Alert, Request},
};

pub fn can_present(alert: &Alert, ringing: &[CallId]) -> bool {
    alert.call_id.is_none_or(|id| ringing.contains(&id))
}

#[derive(Clone, Default)]
pub struct PreferenceQueue(VecDeque<Request>);
impl PreferenceQueue {
    pub fn push(&mut self, request: Request) -> Result<(), &'static str> {
        if self.0.len() >= 32 {
            return Err("Too many pending notification changes. Try again after saving.");
        }
        self.0.push_back(request);
        Ok(())
    }
    pub fn next(&mut self, revision: i64) -> Option<Request> {
        let mut request = self.0.pop_front()?;
        match &mut request {
            Request::SetDefault { revision: r, .. }
            | Request::SetSubscription { revision: r, .. }
            | Request::RemoveSubscription { revision: r, .. } => *r = revision,
            _ => {}
        }
        Some(request)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thiscord_shared::notifications::{Delivery, Kind, Snooze, Target};
    #[test]
    fn delayed_native_failure_does_not_restore_an_ended_call_in_app() {
        let call = "11111111-1111-4111-8111-111111111111".parse().unwrap();
        let conversation = "22222222-2222-4222-8222-222222222222".parse().unwrap();
        let alert = Alert {
            id: format!("{call}"),
            target: Target::Conversation {
                conversation_id: conversation,
            },
            kind: Kind::IncomingCall,
            title: "Incoming call".into(),
            delivery: Delivery::default_for(Kind::IncomingCall),
            actor: None,
            call_id: Some(call),
            version: None,
        };
        assert!(can_present(&alert, &[call]));
        // Native show has been started; a pushed end arrives before its error.
        assert!(!can_present(&alert, &[]));
    }
    #[test]
    fn snooze_waits_behind_save_and_later_edits_use_returned_revision() {
        let mut queue = PreferenceQueue::default();
        queue
            .push(Request::SetDefault {
                revision: 0,
                kind: Kind::Update,
                delivery: Delivery::default_for(Kind::Update),
            })
            .unwrap();
        assert!(matches!(
            queue.next(7),
            Some(Request::SetDefault { revision: 7, .. })
        ));
        // First command is in flight when a banner snooze and another edit arrive.
        queue
            .push(Request::Snooze {
                target: Target::Updates,
                duration: Snooze::Hour1,
            })
            .unwrap();
        queue
            .push(Request::RemoveSubscription {
                revision: 7,
                target: Target::Updates,
            })
            .unwrap();
        assert!(matches!(
            queue.next(8),
            Some(Request::Snooze {
                duration: Snooze::Hour1,
                ..
            })
        ));
        assert!(matches!(
            queue.next(9),
            Some(Request::RemoveSubscription { revision: 9, .. })
        ));
        assert!(queue.next(10).is_none());
    }
}
