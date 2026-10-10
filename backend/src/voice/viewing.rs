use super::*;
use thiscord_shared::screen::{Subscription, VIEW_LEASE_SECS};

#[derive(Clone, Copy)]
pub(super) struct View {
    pub subscription: Subscription,
    pub since: Instant,
    pub renewed: Instant,
}
impl View {
    fn live(&self) -> bool {
        self.renewed.elapsed() < Duration::from_secs(VIEW_LEASE_SECS)
    }
}

pub(super) async fn update(room: &Room, slot: usize, views: Vec<Subscription>) -> bool {
    if views.len() >= ROOM_CAPACITY || views.iter().any(|v| !v.valid()) {
        return false;
    }
    let mut seen = [false; ROOM_CAPACITY];
    for view in &views {
        if std::mem::replace(&mut seen[view.slot], true) {
            return false;
        }
    }
    let mut members = room.members.write().await;
    let Some(receiver) = members.get(&slot) else {
        return false;
    };
    if !receiver.subscriptions_enabled {
        return false;
    }
    let generation = receiver.generation.load(Ordering::Acquire);
    let Some(_permit) = access::global().packet(generation, generation) else {
        return true;
    };
    let now = Instant::now();
    let mut next = [None; ROOM_CAPACITY];
    if receiver.active.load(Ordering::Acquire) && !receiver.info.deafened {
        for subscription in views {
            let Some(source) = members.get(&subscription.slot) else {
                continue;
            };
            if source.info.slot == slot
                || source.info.account_id != subscription.owner
                || source.info.screen_epoch != subscription.epoch
                || !source.active.load(Ordering::Acquire)
                || source.generation.load(Ordering::Acquire) != generation
                || !may_publish(&source.info, MediaKind::ScreenVideo)
            {
                continue;
            }
            let since = receiver.views[subscription.slot]
                .filter(|old| {
                    old.live()
                        && old.subscription.owner == subscription.owner
                        && old.subscription.epoch == subscription.epoch
                })
                .map_or(now, |old| old.since);
            next[subscription.slot] = Some(View {
                subscription,
                since,
                renewed: now,
            });
        }
    }
    members.get_mut(&slot).unwrap().views = next;
    true
}

/// Used both before queue admission and on every transport poll, under the room lock.
pub(super) fn receives(
    receiver: &Member,
    source: &Member,
    kind: MediaKind,
    queued: Instant,
) -> bool {
    if !source.active.load(Ordering::Acquire)
        || !may_publish(&source.info, kind)
        || receiver.info.slot == source.info.slot
        || receiver.info.deafened
        || !receiver.active.load(Ordering::Acquire)
        || receiver.generation.load(Ordering::Acquire) != source.generation.load(Ordering::Acquire)
    {
        return false;
    }
    if kind == MediaKind::Microphone {
        return true;
    }
    receiver.views[source.info.slot].is_some_and(|v| {
        v.live()
            && queued >= v.since
            && v.subscription.owner == source.info.account_id
            && v.subscription.epoch == source.info.screen_epoch
    })
}

pub(super) async fn target(room: &Room, slot: usize) -> Option<(u32, u32)> {
    let members = room.members.read().await;
    let source = members.get(&slot)?;
    if !source.subscriptions_enabled
        || !source.active.load(Ordering::Acquire)
        || !may_publish(&source.info, MediaKind::ScreenVideo)
    {
        return None;
    }
    let generation = source.generation.load(Ordering::Acquire);
    let _permit = access::global().packet(generation, generation)?;
    let bitrate = members
        .values()
        .filter(|receiver| receives(receiver, source, MediaKind::ScreenVideo, Instant::now()))
        .filter_map(|receiver| receiver.views[slot].map(|v| v.subscription.bitrate))
        .min()
        .unwrap_or(0);
    Some((source.info.screen_epoch, bitrate))
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn room() -> Room {
        let room = Room::default();
        for slot in 0..3 {
            let (keyframes, _) = mpsc::channel(1);
            room.members.write().await.insert(
                slot,
                Member {
                    network: Arc::new(RwLock::new(Default::default())),
                    info: Participant {
                        account_id: thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4()),
                        username: "viewer".into(),
                        display_name: String::new(),
                        avatar_id: None,
                        slot,
                        muted: false,
                        deafened: false,
                        can_speak: true,
                        sharing_screen: slot == 0,
                        sharing_audio: slot == 0,
                        screen_epoch: 9,
                    },
                    source_id: uuid::Uuid::new_v4(),
                    screen_started: Instant::now(),
                    outgoing: MediaKind::ALL.map(|kind| egress::channel(kind).0),
                    keyframes,
                    last_keyframe: None,
                    feedback_enabled: true,
                    subscriptions_enabled: true,
                    views: [None; ROOM_CAPACITY],
                    generation: Arc::new(AtomicU64::new(access::global().snapshot().unwrap())),
                    metrics: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
                    active: Arc::new(AtomicBool::new(true)),
                },
            );
        }
        room
    }
    async fn subscription(room: &Room) -> Subscription {
        Subscription {
            slot: 0,
            owner: room.members.read().await[&0].info.account_id,
            epoch: 9,
            bitrate: 5_000_000,
        }
    }
    async fn allowed(room: &Room, receiver: usize, kind: MediaKind, queued: Instant) -> bool {
        let members = room.members.read().await;
        receives(&members[&receiver], &members[&0], kind, queued)
    }
    #[tokio::test]
    async fn viewers_control_fanout_shared_audio_and_lowest_budget() {
        let room = room().await;
        let view = subscription(&room).await;
        assert_eq!(target(&room, 0).await, Some((9, 0)));
        assert!(allowed(&room, 1, MediaKind::Microphone, Instant::now()).await);
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(!allowed(&room, 1, MediaKind::SystemAudio, Instant::now()).await);
        assert!(update(&room, 1, vec![view]).await);
        assert!(allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(allowed(&room, 1, MediaKind::SystemAudio, Instant::now()).await);
        assert!(!allowed(&room, 2, MediaKind::ScreenVideo, Instant::now()).await);
        assert_eq!(target(&room, 0).await, Some((9, 5_000_000)));
        assert!(
            update(
                &room,
                2,
                vec![Subscription {
                    bitrate: 500_000,
                    ..view
                }]
            )
            .await
        );
        assert_eq!(target(&room, 0).await, Some((9, 500_000)));
        assert!(update(&room, 2, vec![]).await);
        assert_eq!(target(&room, 0).await, Some((9, 5_000_000)));
        let queued = Instant::now();
        assert!(update(&room, 1, vec![]).await);
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, queued).await);
        assert_eq!(target(&room, 0).await, Some((9, 0)));
        assert!(update(&room, 1, vec![view]).await);
        assert!(
            !allowed(&room, 1, MediaKind::ScreenVideo, queued).await,
            "old queued packets cannot reach a new viewer"
        );
    }
    #[tokio::test]
    async fn stale_leases_identity_epochs_deafen_and_access_fail_closed() {
        let room = room().await;
        let view = subscription(&room).await;
        assert!(update(&room, 1, vec![view]).await);
        room.members.write().await.get_mut(&1).unwrap().views[0]
            .as_mut()
            .unwrap()
            .renewed -= Duration::from_secs(VIEW_LEASE_SECS);
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert_eq!(target(&room, 0).await, Some((9, 0)));
        assert!(update(&room, 1, vec![view]).await);
        update_voice_state(&room, 1, false, true).await;
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        update_voice_state(&room, 1, false, false).await;
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(update(&room, 1, vec![view]).await);
        let old_ingress = Instant::now();
        update_screen_state(&room, 0, false, false).await.unwrap();
        update_screen_state(&room, 0, true, true).await.unwrap();
        assert!(!may_relay(
            &room.members.read().await[&0],
            MediaKind::ScreenVideo,
            old_ingress
        ));
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(update(&room, 1, vec![view]).await); // Stale share epoch is ignored.
        assert!(room.members.read().await[&1].views[0].is_none());
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .screen_epoch = 9;
        assert!(update(&room, 1, vec![view]).await);
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .account_id = thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4());
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .account_id = view.owner;
        room.members.read().await[&1]
            .generation
            .fetch_add(1, Ordering::Release);
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        room.members.read().await[&1]
            .generation
            .fetch_sub(1, Ordering::Release);
        room.members.read().await[&1]
            .active
            .store(false, Ordering::Release);
        assert_eq!(target(&room, 0).await, Some((9, 0)));
    }
    #[tokio::test]
    async fn malformed_cross_room_and_self_subscriptions_never_grant_delivery() {
        let room = room().await;
        let view = subscription(&room).await;
        assert!(!update(&room, 1, vec![view; ROOM_CAPACITY]).await);
        assert!(!update(&room, 1, vec![view, view]).await);
        assert!(!update(&room, 1, vec![Subscription { bitrate: 0, ..view }]).await);
        assert!(
            !update(
                &room,
                1,
                vec![Subscription {
                    slot: ROOM_CAPACITY,
                    ..view
                }]
            )
            .await
        );
        assert!(update(&room, 0, vec![view]).await);
        assert!(room.members.read().await[&0].views[0].is_none());
        assert!(
            update(
                &room,
                1,
                vec![Subscription {
                    owner: thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4()),
                    ..view
                }]
            )
            .await
        );
        assert_eq!(target(&room, 0).await, Some((9, 0)));
        room.members
            .write()
            .await
            .get_mut(&1)
            .unwrap()
            .subscriptions_enabled = false;
        assert!(!update(&room, 1, vec![view]).await);
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .subscriptions_enabled = false;
        assert!(update_screen_state(&room, 0, true, true).await.is_err());
    }
}
