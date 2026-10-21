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

pub(super) async fn update(
    room: &Room,
    slot: usize,
    views: Vec<Subscription>,
    owner: &AtomicBool,
) -> bool {
    if views.len() > (ROOM_CAPACITY - 1) * 2 || views.iter().any(|v| !v.valid()) {
        return false;
    }
    let mut seen = [false; ROOM_CAPACITY * 2];
    for view in &views {
        if std::mem::replace(&mut seen[view.kind.view_index(view.slot)], true) {
            return false;
        }
    }
    let mut members = room.members.write().await;
    if !owner.load(Ordering::Acquire) {
        return false;
    }
    let Some(receiver) = members.get(&slot) else {
        return false;
    };
    if !receiver.subscriptions_enabled {
        return false;
    }
    let generation = receiver.generation.load(Ordering::Acquire);
    let Some(_permit) = receiver.access.packet(generation, generation) else {
        return true;
    };
    let now = Instant::now();
    let mut next = [None; ROOM_CAPACITY * 2];
    let mut source_permits = Vec::new();
    if receiver.active.load(Ordering::Acquire) && !receiver.info.deafened {
        for subscription in views {
            let Some(source) = members.get(&subscription.slot) else {
                continue;
            };
            if source.info.slot == slot
                || source.info.account_id != subscription.owner
                || source.info.video_epoch(subscription.kind) != subscription.epoch
                || !source.active.load(Ordering::Acquire)
                || !may_publish(&source.info, subscription.kind)
            {
                continue;
            }
            let epoch = source.generation.load(Ordering::Acquire);
            let Some(permit) = source.access.packet(epoch, epoch) else {
                continue;
            };
            source_permits.push(permit);
            let since = receiver.views[subscription.kind.view_index(subscription.slot)]
                .filter(|old| {
                    old.live()
                        && old.subscription.owner == subscription.owner
                        && old.subscription.epoch == subscription.epoch
                })
                .map_or(now, |old| old.since);
            next[subscription.kind.view_index(subscription.slot)] = Some(View {
                subscription,
                since,
                renewed: now,
            });
        }
    }
    {
        let mut routes = receiver.screen_routes.lock().unwrap();
        for source in 0..ROOM_CAPACITY {
            if next[source]
                .is_none_or(|v| receiver.views[source].is_none_or(|old| old.since != v.since))
            {
                routes[source].clear();
            }
        }
    }
    members.get_mut(&slot).unwrap().views = next;
    true
}

/// Used both before queue admission and on every transport poll, under the room lock.
pub(super) fn within_budget(
    receiver: &Member,
    source: &Member,
    kind: MediaKind,
    bitrate: Option<u32>,
) -> bool {
    if kind != MediaKind::ScreenVideo || !source.screen_layers {
        return true;
    }
    bitrate.is_some_and(|rate| {
        receiver.views[source.info.slot].is_some_and(|v| rate <= v.subscription.bitrate)
    })
}

/// Used both before queue admission and on every transport poll, under the room lock.
pub(super) fn receives(
    receiver: &Member,
    source: &Member,
    kind: MediaKind,
    queued: Instant,
) -> bool {
    if !source.negotiated
        || !receiver.negotiated
        || !source.active.load(Ordering::Acquire)
        || !may_publish(&source.info, kind)
        || receiver.info.slot == source.info.slot
        || receiver.info.deafened
        || !receiver.active.load(Ordering::Acquire)
        || !receiver
            .access
            .current(receiver.generation.load(Ordering::Acquire))
        || !source
            .access
            .current(source.generation.load(Ordering::Acquire))
    {
        return false;
    }
    if kind == MediaKind::Microphone {
        return true;
    }
    receiver.views[kind.view_index(source.info.slot)].is_some_and(|v| {
        v.live()
            && queued >= v.since
            && v.subscription.owner == source.info.account_id
            && v.subscription.epoch == source.info.video_epoch(kind)
    })
}

pub(super) async fn target(room: &Room, slot: usize, kind: MediaKind) -> Option<(u32, u32)> {
    let members = room.members.read().await;
    let source = members.get(&slot)?;
    if !source.subscriptions_enabled
        || !source.active.load(Ordering::Acquire)
        || !may_publish(&source.info, kind)
    {
        return None;
    }
    let generation = source.generation.load(Ordering::Acquire);
    let _permit = source.access.packet(generation, generation)?;
    let mut receiver_permits = Vec::new();
    let bitrate = members
        .values()
        .filter(|receiver| receives(receiver, source, kind, Instant::now()))
        .filter_map(|receiver| {
            let epoch = receiver.generation.load(Ordering::Acquire);
            receiver_permits.push(receiver.access.packet(epoch, epoch)?);
            receiver.views[kind.view_index(slot)].map(|v| v.subscription.bitrate)
        })
        .min()
        .unwrap_or(0);
    Some((source.info.video_epoch(kind), bitrate))
}

pub(super) fn layer_plan(
    members: &HashMap<usize, Member>,
    source: &Member,
) -> thiscord_shared::screen::LayerTargets {
    thiscord_shared::screen::LayerTargets::plan(
        source.screen_quality,
        members
            .values()
            .filter(|receiver| receives(receiver, source, MediaKind::ScreenVideo, Instant::now()))
            .filter_map(|receiver| {
                receiver.views[source.info.slot].map(|v| v.subscription.bitrate)
            }),
    )
}

pub(super) async fn layer_target(
    room: &Room,
    slot: usize,
    kind: MediaKind,
) -> Option<thiscord_shared::screen::LayerTargets> {
    if kind != MediaKind::ScreenVideo {
        return None;
    }
    let members = room.members.read().await;
    let source = members.get(&slot)?;
    if !source.screen_layers {
        return None;
    }
    for receiver in members.values() {
        if !receives(receiver, source, kind, Instant::now()) {
            receiver.screen_routes.lock().unwrap()[slot].clear();
        }
    }
    Some(layer_plan(&members, source))
}

/// Cleanup also runs for receivers whose source has left or stopped publishing.
pub(super) async fn prune_routes(room: &Room, slot: usize) {
    let members = room.members.read().await;
    let Some(receiver) = members.get(&slot) else {
        return;
    };
    let mut routes = receiver.screen_routes.lock().unwrap();
    for (slot, route) in routes.iter_mut().enumerate() {
        if members.get(&slot).is_none_or(|source| {
            !receives(receiver, source, MediaKind::ScreenVideo, Instant::now())
        }) {
            route.clear();
        } else {
            route.expire();
        }
    }
}

/// Derive viewer presence from the same live, authorized leases used for media delivery.
/// Never cache it in Member.info or include it in the outside-call sidebar roster.
pub(super) fn participants(members: &HashMap<usize, Member>) -> Vec<Participant> {
    let now = Instant::now();
    members
        .values()
        .filter(|source| source.negotiated)
        .map(|source| {
            let viewers = |kind| {
                let mut ids: Vec<_> = members
                    .values()
                    .filter(|receiver| receives(receiver, source, kind, now))
                    .map(|receiver| receiver.info.account_id)
                    .collect();
                ids.sort();
                ids
            };
            let mut info = red::participant(source);
            info.screen_viewers = viewers(MediaKind::ScreenVideo);
            info.camera_viewers = viewers(MediaKind::Camera);
            info
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    async fn room() -> Room {
        let room = Room::default();
        for slot in 0..3 {
            let (keyframes, _) = mpsc::channel(1);
            let account = thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4());
            let access = access::register(access::Identity {
                account,
                session: thiscord_shared::SessionId::from_uuid(uuid::Uuid::new_v4()),
                guild: None,
            });
            room.members.write().await.insert(
                slot,
                Member {
                    resumption: None,
                    request_id: thiscord_shared::RequestId::from_uuid(uuid::Uuid::new_v4()),
                    call_id: None,
                    replaced: watch::channel(false).0,
                    network: Arc::new(RwLock::new(Default::default())),
                    info: Participant {
                        account_id: account,
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
                        microphone_epoch: 9,
                        microphone_source_epoch: 9,
                        camera_active: false,
                        camera_epoch: 0,
                        screen_viewers: vec![],
                        camera_viewers: vec![],
                    },
                    source_id: uuid::Uuid::new_v4(),
                    screen_started: Instant::now(),
                    microphone_started: Instant::now(),
                    negotiated: true,
                    packet_redundancy: false,
                    expected_sender_generation: None,
                    restricted_sender_generation: None,
                    red: red::Barrier::default(),
                    camera_started: Instant::now(),
                    outgoing: MediaKind::ALL.map(|kind| egress::channel(kind).0),
                    keyframes,
                    last_keyframe: [None; 2],
                    feedback_enabled: true,
                    subscriptions_enabled: true,
                    screen_layers: false,
                    screen_quality: Default::default(),
                    screen_routes: Default::default(),
                    views: [None; ROOM_CAPACITY * 2],
                    generation: Arc::new(AtomicU64::new(0)),
                    access,
                    metrics: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
                    active: Arc::new(AtomicBool::new(true)),
                },
            );
        }
        room
    }
    async fn subscription(room: &Room) -> Subscription {
        Subscription {
            kind: thiscord_shared::voice::MediaKind::ScreenVideo,
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
    async fn audience(room: &Room) -> Participant {
        participants(&*room.members.read().await)
            .into_iter()
            .find(|p| p.slot == 0)
            .unwrap()
    }
    #[tokio::test]
    async fn viewer_presence_follows_kind_epoch_hide_expiry_deafen_and_access() {
        let room = room().await;
        let screen = subscription(&room).await;
        {
            let mut members = room.members.write().await;
            let source = &mut members.get_mut(&0).unwrap().info;
            source.camera_active = true;
            source.camera_epoch = 23;
        }
        let camera = Subscription {
            kind: MediaKind::Camera,
            epoch: 23,
            ..screen
        };
        let live = AtomicBool::new(true);
        let viewer = room.members.read().await[&1].info.account_id;
        assert!(update(&room, 1, vec![screen, camera], &live).await);
        let info = audience(&room).await;
        assert_eq!(info.screen_viewers, [viewer]);
        assert_eq!(info.camera_viewers, [viewer]);
        assert!(
            room.members.read().await[&0].info.screen_viewers.is_empty(),
            "sidebar roster must not inherit viewer presence"
        );
        assert!(update(&room, 1, vec![camera], &live).await);
        assert!(audience(&room).await.screen_viewers.is_empty());
        assert_eq!(audience(&room).await.camera_viewers, [viewer]);
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .camera_epoch += 1;
        assert!(audience(&room).await.camera_viewers.is_empty());
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .camera_epoch = 23;
        room.members.write().await.get_mut(&1).unwrap().views[MediaKind::Camera.view_index(0)]
            .as_mut()
            .unwrap()
            .renewed -= Duration::from_secs(VIEW_LEASE_SECS);
        assert!(audience(&room).await.camera_viewers.is_empty());
        assert!(update(&room, 1, vec![camera], &live).await);
        let revoked = access::pause(access::Scope::Account(viewer));
        assert!(audience(&room).await.camera_viewers.is_empty());
        revoked.finish(false);
        assert!(audience(&room).await.camera_viewers.is_empty());
        {
            let members = room.members.read().await;
            let receiver = &members[&1];
            receiver
                .generation
                .store(receiver.access.snapshot().unwrap(), Ordering::Release);
        }
        assert_eq!(audience(&room).await.camera_viewers, [viewer]);
        let revoked = access::pause(access::Scope::Account(screen.owner));
        assert!(audience(&room).await.camera_viewers.is_empty());
        revoked.finish(false);
        {
            let members = room.members.read().await;
            let source = &members[&0];
            source
                .generation
                .store(source.access.snapshot().unwrap(), Ordering::Release);
        }
        update_voice_state(&room, 1, false, true, &live).await;
        assert!(audience(&room).await.camera_viewers.is_empty());
        update_voice_state(&room, 1, false, false, &live).await;
        assert!(update(&room, 1, vec![camera], &live).await);
        room.members
            .write()
            .await
            .get_mut(&1)
            .unwrap()
            .active
            .store(false, Ordering::Release);
        assert!(audience(&room).await.camera_viewers.is_empty());
    }
    #[tokio::test]
    async fn viewer_admission_and_bitrate_validate_each_endpoint_epoch() {
        for changed_slot in [0, 1] {
            let room = room().await;
            let view = subscription(&room).await;
            assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
            let account = room.members.read().await[&changed_slot].info.account_id;
            let mutation = access::pause(access::Scope::Account(account));
            for kind in [
                MediaKind::Microphone,
                MediaKind::ScreenVideo,
                MediaKind::SystemAudio,
            ] {
                assert!(!allowed(&room, 1, kind, Instant::now()).await);
            }
            assert_eq!(
                target(&room, 0, MediaKind::ScreenVideo).await,
                if changed_slot == 0 {
                    None
                } else {
                    Some((9, 0))
                }
            );
            mutation.finish(true);
            // A completed mutation still needs a fresh authoritative grant.
            assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
            assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
            {
                let members = room.members.read().await;
                let endpoint = &members[&changed_slot];
                endpoint
                    .generation
                    .store(endpoint.access.snapshot().unwrap(), Ordering::Release);
                assert_ne!(
                    members[&0].generation.load(Ordering::Acquire),
                    members[&1].generation.load(Ordering::Acquire)
                );
            }
            assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
            for kind in [
                MediaKind::Microphone,
                MediaKind::ScreenVideo,
                MediaKind::SystemAudio,
            ] {
                assert!(
                    allowed(&room, 1, kind, Instant::now()).await,
                    "valid unequal epochs rejected: {kind:?}"
                );
            }
            assert_eq!(
                target(&room, 0, MediaKind::ScreenVideo).await,
                Some((9, 5_000_000))
            );
        }
    }

    #[tokio::test]
    async fn viewers_control_fanout_shared_audio_and_lowest_budget() {
        let room = room().await;
        let view = subscription(&room).await;
        assert_eq!(target(&room, 0, MediaKind::ScreenVideo).await, Some((9, 0)));
        assert!(allowed(&room, 1, MediaKind::Microphone, Instant::now()).await);
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(!allowed(&room, 1, MediaKind::SystemAudio, Instant::now()).await);
        assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
        assert!(allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(allowed(&room, 1, MediaKind::SystemAudio, Instant::now()).await);
        assert!(!allowed(&room, 2, MediaKind::ScreenVideo, Instant::now()).await);
        assert_eq!(
            target(&room, 0, MediaKind::ScreenVideo).await,
            Some((9, 5_000_000))
        );
        assert!(
            update(
                &room,
                2,
                vec![Subscription {
                    bitrate: 500_000,
                    ..view
                }],
                &AtomicBool::new(true)
            )
            .await
        );
        assert_eq!(
            target(&room, 0, MediaKind::ScreenVideo).await,
            Some((9, 500_000))
        );
        assert!(update(&room, 2, vec![], &AtomicBool::new(true)).await);
        assert_eq!(
            target(&room, 0, MediaKind::ScreenVideo).await,
            Some((9, 5_000_000))
        );
        let queued = Instant::now();
        assert!(update(&room, 1, vec![], &AtomicBool::new(true)).await);
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, queued).await);
        assert_eq!(target(&room, 0, MediaKind::ScreenVideo).await, Some((9, 0)));
        assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
        assert!(
            !allowed(&room, 1, MediaKind::ScreenVideo, queued).await,
            "old queued packets cannot reach a new viewer"
        );
    }
    #[tokio::test]
    async fn layered_demand_keeps_fast_quality_and_expires_with_authorized_leases() {
        let room = room().await;
        let view = subscription(&room).await;
        let live = AtomicBool::new(true);
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .screen_layers = true;
        assert!(update(&room, 1, vec![view], &live).await);
        assert!(
            update(
                &room,
                2,
                vec![Subscription {
                    bitrate: 200_000,
                    ..view
                }],
                &live
            )
            .await
        );
        let plan = layer_target(&room, 0, MediaKind::ScreenVideo)
            .await
            .unwrap();
        assert_eq!(plan.primary, 5_000_000);
        assert_eq!(plan.low, 200_000);
        assert_eq!(plan.layer(view.bitrate), 0);
        assert_eq!(plan.layer(200_000), 1);
        // Legacy participants can receive either encoded layer without new wire fields.
        assert!(!room.members.read().await[&2].screen_layers);
        assert!(allowed(&room, 2, MediaKind::SystemAudio, Instant::now()).await);
        assert_eq!(layer_target(&room, 0, MediaKind::Camera).await, None);
        room.members.write().await.get_mut(&2).unwrap().views[0]
            .as_mut()
            .unwrap()
            .renewed -= Duration::from_secs(VIEW_LEASE_SECS);
        assert_eq!(
            layer_target(&room, 0, MediaKind::ScreenVideo)
                .await
                .unwrap()
                .low,
            0
        );
        assert!(
            update(
                &room,
                2,
                vec![Subscription {
                    bitrate: 200_000,
                    ..view
                }],
                &live
            )
            .await
        );
        {
            let members = room.members.read().await;
            members[&2].active.store(false, Ordering::Release);
        }
        assert_eq!(
            layer_target(&room, 0, MediaKind::ScreenVideo)
                .await
                .unwrap()
                .low,
            0
        );
        assert!(update(&room, 1, vec![], &live).await);
        assert_eq!(
            layer_target(&room, 0, MediaKind::ScreenVideo)
                .await
                .unwrap(),
            Default::default()
        );
        // Nonnegotiated publishers retain the established minimum-budget behavior.
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .screen_layers = false;
        assert_eq!(layer_target(&room, 0, MediaKind::ScreenVideo).await, None);
    }
    #[tokio::test]
    async fn stale_leases_identity_epochs_deafen_and_access_fail_closed() {
        let room = room().await;
        let view = subscription(&room).await;
        assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
        room.members.write().await.get_mut(&1).unwrap().views[0]
            .as_mut()
            .unwrap()
            .renewed -= Duration::from_secs(VIEW_LEASE_SECS);
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert_eq!(target(&room, 0, MediaKind::ScreenVideo).await, Some((9, 0)));
        assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
        update_voice_state(&room, 1, false, true, &AtomicBool::new(true)).await;
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        update_voice_state(&room, 1, false, false, &AtomicBool::new(true)).await;
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
        let old_ingress = Instant::now();
        update_screen_state(&room, 0, false, false, &AtomicBool::new(true))
            .await
            .unwrap();
        update_screen_state(&room, 0, true, true, &AtomicBool::new(true))
            .await
            .unwrap();
        assert!(!may_relay(
            &room.members.read().await[&0],
            MediaKind::ScreenVideo,
            old_ingress
        ));
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await); // Stale share epoch is ignored.
        assert!(room.members.read().await[&1].views[0].is_none());
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .screen_epoch = 9;
        assert!(update(&room, 1, vec![view], &AtomicBool::new(true)).await);
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
        assert_eq!(target(&room, 0, MediaKind::ScreenVideo).await, Some((9, 0)));
    }
    #[tokio::test]
    async fn malformed_cross_room_and_self_subscriptions_never_grant_delivery() {
        let room = room().await;
        let view = subscription(&room).await;
        assert!(!update(&room, 1, vec![view; ROOM_CAPACITY], &AtomicBool::new(true)).await);
        assert!(!update(&room, 1, vec![view, view], &AtomicBool::new(true)).await);
        assert!(
            !update(
                &room,
                1,
                vec![Subscription { bitrate: 0, ..view }],
                &AtomicBool::new(true)
            )
            .await
        );
        assert!(
            !update(
                &room,
                1,
                vec![Subscription {
                    slot: ROOM_CAPACITY,
                    ..view
                }],
                &AtomicBool::new(true)
            )
            .await
        );
        assert!(update(&room, 0, vec![view], &AtomicBool::new(true)).await);
        assert!(room.members.read().await[&0].views[0].is_none());
        assert!(
            update(
                &room,
                1,
                vec![Subscription {
                    owner: thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4()),
                    ..view
                }],
                &AtomicBool::new(true)
            )
            .await
        );
        assert_eq!(target(&room, 0, MediaKind::ScreenVideo).await, Some((9, 0)));
        room.members
            .write()
            .await
            .get_mut(&1)
            .unwrap()
            .subscriptions_enabled = false;
        assert!(!update(&room, 1, vec![view], &AtomicBool::new(true)).await);
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .subscriptions_enabled = false;
        assert!(
            update_screen_state(&room, 0, true, true, &AtomicBool::new(true))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn camera_and_screen_views_are_independent_and_access_fenced() {
        let room = room().await;
        let screen = subscription(&room).await;
        {
            let mut members = room.members.write().await;
            let p = &mut members.get_mut(&0).unwrap().info;
            p.camera_active = true;
            p.camera_epoch = 23;
        }
        let camera = Subscription {
            kind: MediaKind::Camera,
            epoch: 23,
            ..screen
        };
        let live = AtomicBool::new(true);
        assert!(update(&room, 1, vec![screen, camera], &live).await);
        for kind in MediaKind::ALL {
            assert!(allowed(&room, 1, kind, Instant::now()).await);
        }
        assert!(update(&room, 1, vec![camera], &live).await);
        assert!(!allowed(&room, 1, MediaKind::ScreenVideo, Instant::now()).await);
        assert!(!allowed(&room, 1, MediaKind::SystemAudio, Instant::now()).await);
        assert!(allowed(&room, 1, MediaKind::Camera, Instant::now()).await);
        assert!(!update(&room, 1, vec![camera, camera], &live).await);
        assert!(
            !update(
                &room,
                1,
                vec![Subscription {
                    kind: MediaKind::Microphone,
                    ..camera
                }],
                &live
            )
            .await
        );
        let old = Instant::now();
        assert!(update(&room, 1, vec![], &live).await);
        assert!(update(&room, 1, vec![camera], &live).await);
        assert!(!allowed(&room, 1, MediaKind::Camera, old).await);
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .camera_epoch += 1;
        assert!(!allowed(&room, 1, MediaKind::Camera, Instant::now()).await);
        assert!(update(&room, 1, vec![camera], &live).await);
        assert!(room.members.read().await[&1].views[MediaKind::Camera.view_index(0)].is_none());
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .camera_epoch = 23;
        assert!(update(&room, 1, vec![camera], &live).await);
        let account = room.members.read().await[&0].info.account_id;
        let mutation = access::pause(access::Scope::Account(account));
        assert!(!allowed(&room, 1, MediaKind::Camera, Instant::now()).await);
        mutation.finish(false);
        room.members
            .write()
            .await
            .get_mut(&0)
            .unwrap()
            .info
            .can_speak = false;
        assert!(!allowed(&room, 1, MediaKind::Camera, Instant::now()).await);
        update_voice_state(&room, 1, false, true, &live).await;
        assert!(
            room.members.read().await[&1]
                .views
                .iter()
                .all(Option::is_none)
        );
    }
}
