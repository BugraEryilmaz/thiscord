use super::*;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Rejected {
    Full,
    Replaced,
    StaleGrant,
}

pub(super) async fn admit(
    room: &Room,
    mut member: Member,
    reconnect: bool,
) -> Result<usize, Rejected> {
    let mut members = room.members.write().await;
    // Authorization and replacement commit together. A revoked join must never
    // displace a valid connection, even if its DB check completed earlier.
    let epoch = member.generation.load(Ordering::Acquire);
    let access = member.access.clone();
    let _admission = access.packet(epoch, epoch).ok_or(Rejected::StaleGrant)?;
    let previous_slot = members.iter().find_map(|(&slot, previous)| {
        (previous.info.account_id == member.info.account_id).then_some(slot)
    });
    if let Some(previous) = previous_slot.and_then(|slot| members.get(&slot))
        && reconnect
        && (member.call_id.is_none() || previous.call_id != member.call_id)
    {
        return Err(Rejected::Replaced);
    }
    let slot = previous_slot
        .or_else(|| (0..ROOM_CAPACITY).find(|s| !members.contains_key(s)))
        .ok_or(Rejected::Full)?;
    if let Some(previous) = members.get(&slot) {
        tracing::info!(request_id = %member.request_id, previous_request_id = %previous.request_id, previous_active = previous.active.load(Ordering::Acquire), "voice connection replaced");
        // Media writes and state mutations check this under the room lock.
        previous.active.store(false, Ordering::Release);
        previous.replaced.send_replace(true);
    }
    member.info.slot = slot;
    if previous_slot.is_none() {
        voice_notification(
            room,
            &members,
            &member.info,
            thiscord_shared::notifications::Kind::VoiceJoin,
        );
    }
    members.insert(slot, member);
    room.changed.send_replace(());
    Ok(slot)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(account: thiscord_shared::AccountId, call_id: RequestId) -> Member {
        let access = access::Access::new();
        Member {
            resumption: None,
            request_id: RequestId::from_uuid(uuid::Uuid::new_v4()),
            call_id: Some(call_id),
            replaced: watch::channel(false).0,
            network: Default::default(),
            info: Participant {
                test_bot: false,
                account_id: account,
                username: "test".into(),
                display_name: "test".into(),
                avatar_id: None,
                slot: 0,
                muted: false,
                deafened: false,
                can_speak: true,
                sharing_screen: false,
                sharing_audio: false,
                screen_epoch: 1,
                microphone_epoch: 1,
                microphone_source_epoch: 1,
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
            keyframes: mpsc::channel(1).0,
            last_keyframe: [None; 2],
            feedback_enabled: true,
            subscriptions_enabled: true,
            screen_layers: false,
            screen_quality: Default::default(),
            screen_routes: Default::default(),
            views: [None; ROOM_CAPACITY * 2],
            generation: Arc::new(AtomicU64::new(access.snapshot().unwrap())),
            access,
            metrics: Arc::new(std::array::from_fn(|_| AtomicU64::new(0))),
            active: Arc::new(AtomicBool::new(true)),
        }
    }
    fn id() -> RequestId {
        RequestId::from_uuid(uuid::Uuid::new_v4())
    }
    fn account() -> thiscord_shared::AccountId {
        thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4())
    }

    #[tokio::test]
    async fn reused_slot_is_not_advertised_as_legacy_during_negotiation() {
        let room = Room::default();
        let mut original = member(account(), id());
        original.packet_redundancy = true;
        let slot = admit(&room, original.clone(), false).await.unwrap();
        let mut listener = member(account(), id());
        listener.packet_redundancy = true;
        admit(&room, listener.clone(), false).await.unwrap();
        assert_eq!(
            viewing::participants(&*room.members.read().await)[0].microphone_source_epoch,
            1
        );
        release_member(&room, slot, original.source_id).await;

        let mut pending = member(account(), id());
        pending.negotiated = false;
        let pending_id = pending.info.account_id;
        assert_eq!(admit(&room, pending, false).await.unwrap(), slot);
        let mut members = room.members.write().await;
        let roster = viewing::participants(&members);
        assert_eq!(roster.len(), 1);
        assert_eq!(roster[0].account_id, listener.info.account_id);
        assert!(!viewing::receives(
            &listener,
            &members[&slot],
            MediaKind::Microphone,
            Instant::now()
        ));

        let source = members.get_mut(&slot).unwrap();
        source.packet_redundancy = true;
        source.info.microphone_source_epoch = 2;
        source.info.microphone_epoch = 3;
        source.negotiated = true;
        let roster = viewing::participants(&members);
        let source = roster.iter().find(|m| m.account_id == pending_id).unwrap();
        assert_eq!(source.microphone_source_epoch, 2);
        assert_eq!(source.microphone_epoch, 3);
        // A genuinely negotiated old peer must still use the epoch-zero wire shape.
        members.get_mut(&slot).unwrap().packet_redundancy = false;
        let roster = viewing::participants(&members);
        let source = roster.iter().find(|m| m.account_id == pending_id).unwrap();
        assert_eq!(source.microphone_source_epoch, 0);
        assert_eq!(source.microphone_epoch, 0);
    }

    #[tokio::test]
    async fn notification_membership_push_excludes_self_replacements_and_stale_cleanup() {
        use thiscord_shared::notifications::Kind;
        let room = Room {
            notification_target: Some((
                GuildId::from_uuid(uuid::Uuid::new_v4()),
                ChannelId::from_uuid(uuid::Uuid::new_v4()),
            )),
            ..Default::default()
        };
        let a = member(account(), id());
        let b = member(account(), id());
        let mut events = crate::notifications::test_listener(a.info.account_id);
        admit(&room, a, false).await.unwrap();
        assert!(events.receiver.try_recv().is_err());
        let slot = admit(&room, b.clone(), false).await.unwrap();
        assert_eq!(events.receiver.recv().await.unwrap().kind, Kind::VoiceJoin);
        let replacement = member(b.info.account_id, id());
        admit(&room, replacement.clone(), false).await.unwrap();
        release_member(&room, slot, b.source_id).await;
        assert!(events.receiver.try_recv().is_err());
        release_member(&room, slot, replacement.source_id).await;
        assert_eq!(events.receiver.recv().await.unwrap().kind, Kind::VoiceLeave);
    }

    #[tokio::test]
    async fn full_room_replacement_fences_old_cleanup_state_and_retry() {
        let room = Room::default();
        let old = member(account(), id());
        let slot = admit(&room, old.clone(), false).await.unwrap();
        for _ in 1..ROOM_CAPACITY {
            admit(&room, member(account(), id()), false).await.unwrap();
        }
        assert_eq!(
            admit(&room, member(account(), id()), false).await,
            Err(Rejected::Full)
        );
        let new = member(old.info.account_id, id());
        assert_eq!(admit(&room, new.clone(), false).await, Ok(slot));
        assert!(!old.active.load(Ordering::Acquire));
        assert!(*old.replaced.borrow());
        assert!(new.active.load(Ordering::Acquire));
        release_member(&room, slot, old.source_id).await;
        update_voice_state(&room, slot, true, true, &old.active).await;
        assert!(
            update_screen_state(&room, slot, true, true, &old.active)
                .await
                .is_err()
        );
        assert!(!viewing::update(&room, slot, vec![], &old.active).await);
        let members = room.members.read().await;
        assert_eq!(members.len(), ROOM_CAPACITY);
        assert_eq!(members[&slot].source_id, new.source_id);
        assert!(!members[&slot].info.muted);
        drop(members);
        assert_eq!(
            admit(
                &room,
                member(old.info.account_id, old.call_id.unwrap()),
                true
            )
            .await,
            Err(Rejected::Replaced)
        );
        assert!(new.active.load(Ordering::Acquire));
        // The new call can resume its own stale socket without waiting for timeout.
        assert_eq!(
            admit(
                &room,
                member(new.info.account_id, new.call_id.unwrap()),
                true
            )
            .await,
            Ok(slot)
        );
        assert!(!new.active.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn stale_authorization_cannot_displace_an_existing_member() {
        let room = Room::default();
        let old = member(account(), id());
        admit(&room, old.clone(), false).await.unwrap();
        let stale = member(old.info.account_id, id());
        stale.generation.fetch_add(1, Ordering::Release);
        assert_eq!(admit(&room, stale, false).await, Err(Rejected::StaleGrant));
        assert!(old.active.load(Ordering::Acquire));
        assert!(!*old.replaced.borrow());
    }

    #[tokio::test]
    async fn concurrent_joins_leave_one_owner_whose_cleanup_cannot_be_stolen() {
        let room = Room::default();
        let a = member(account(), id());
        let b = member(a.info.account_id, id());
        let (first, second) = tokio::join!(
            admit(&room, a.clone(), false),
            admit(&room, b.clone(), false)
        );
        assert_eq!(first, second);
        let slot = first.unwrap();
        let (winner, loser) = if a.active.load(Ordering::Acquire) {
            (&a, &b)
        } else {
            (&b, &a)
        };
        assert!(!loser.active.load(Ordering::Acquire));
        release_member(&room, slot, loser.source_id).await;
        assert_eq!(room.members.read().await[&slot].source_id, winner.source_id);
    }
}
