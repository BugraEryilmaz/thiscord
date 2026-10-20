//! Independent, bounded media writers. Screen traffic never waits for driver capacity.
use super::*;
use rtc::shared::marshal::MarshalSize;
use std::{future::Future, task::Poll};
use tokio::sync::OwnedSemaphorePermit;

const AUDIO_AGE: Duration = Duration::from_millis(100);
const VIDEO_AGE: Duration = Duration::from_millis(150);
const SCREEN_BACKOFF: Duration = Duration::from_millis(20);

fn max_age(kind: MediaKind) -> Duration {
    if kind.is_video() {
        VIDEO_AGE
    } else {
        AUDIO_AGE
    }
}

#[derive(Clone)]
pub(super) struct Sender {
    tx: mpsc::Sender<Queued>,
    bytes: Arc<Semaphore>,
    kind: MediaKind,
}

pub(super) struct Queued {
    delivery: Delivery,
    // Count queued AND in-flight bytes; cancellation/closure releases the reservation.
    _bytes: OwnedSemaphorePermit,
}

pub(super) fn channel(kind: MediaKind) -> (Sender, mpsc::Receiver<Queued>) {
    let (packets, bytes) = if kind.is_video() {
        (512, 512 * 1024)
    } else {
        (32, 32 * 1024)
    };
    let (tx, rx) = mpsc::channel(packets);
    (
        Sender {
            tx,
            bytes: Arc::new(Semaphore::new(bytes)),
            kind,
        },
        rx,
    )
}

impl Sender {
    /// Called under the room lock and access permit. Intentional filtering is
    /// not congestion and must not request a keyframe or consume queue budget.
    pub(super) fn forward(
        &self,
        source: &Member,
        receiver: &Member,
        delivery: Delivery,
    ) -> Result<(), ()> {
        if delivery.source != source.source_id
            || !delivery.source_active.load(Ordering::Acquire)
            || delivery.epoch != source.generation.load(Ordering::Acquire)
            || delivery.receiver_epoch != receiver.generation.load(Ordering::Acquire)
            || delivery
                .packet
                .header
                .csrc
                .get(if self.kind == MediaKind::Microphone {
                    1
                } else {
                    0
                })
                .copied()
                != Some(if self.kind == MediaKind::Microphone {
                    source.info.microphone_epoch
                } else {
                    source.info.video_epoch(self.kind)
                })
            || (self.kind == MediaKind::Microphone
                && (delivery.queued_at < receiver.red.receiver_started
                    || delivery.packet.header.csrc.first().copied()
                        != Some(source.info.microphone_source_epoch)))
            || !viewing::receives(receiver, source, self.kind, delivery.queued_at)
            || (self.kind == MediaKind::Microphone && !red::receives(source, receiver))
        {
            return Ok(());
        }
        let Some(_permits) = delivery_permits(
            &source.access,
            delivery.epoch,
            &receiver.access,
            delivery.receiver_epoch,
            receiver.generation.load(Ordering::Acquire),
        ) else {
            return Ok(());
        };
        self.try_send(delivery)
    }

    fn try_send(&self, delivery: Delivery) -> Result<(), ()> {
        if delivery.queued_at.elapsed() >= max_age(self.kind) {
            return Err(());
        }
        // Serialized RTP plus conservative SRTP/UDP/IP overhead. Packet count also
        // bounds allocation/queue metadata, independently of payload size.
        let bytes =
            u32::try_from(delivery.packet.marshal_size().saturating_add(64)).map_err(|_| ())?;
        let permit = self
            .bytes
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| ())?;
        self.tx
            .try_send(Queued {
                delivery,
                _bytes: permit,
            })
            .map_err(|_| ())
    }
}

/// The pinned WebRTC track enqueues atomically on its final poll. Cancel a
/// pending screen send immediately, including its driver waiter, so microphone
/// delivery does not depend on completing or timing out a screen write.
async fn write_media<F>(kind: MediaKind, send: F) -> Result<(), ()>
where
    F: Future<Output = Result<(), ()>>,
{
    if kind == MediaKind::Microphone {
        send.await
    } else {
        // One bounded enqueue poll: don't mistake Tokio's cooperative scheduling
        // yield for a full driver queue during an otherwise healthy video burst.
        let mut send = std::pin::pin!(tokio::task::unconstrained(send));
        match std::future::poll_fn(|cx| Poll::Ready(send.as_mut().poll(cx))).await {
            Poll::Ready(result) => result,
            Poll::Pending => Err(()),
        }
    }
}

pub(super) struct Writer {
    pub access: Arc<access::Access>,
    pub room: Arc<Room>,
    pub receiver_slot: usize,
    pub active: Arc<AtomicBool>,
    pub generation: Arc<AtomicU64>,
    pub metrics: Arc<[AtomicU64; 5]>,
}

impl Writer {
    pub(super) async fn run<F, Fut>(self, kind: MediaKind, mut rx: mpsc::Receiver<Queued>, send: F)
    where
        F: Fn(usize, rtp::Packet) -> Fut,
        Fut: Future<Output = Result<(), ()>>,
    {
        let mut sources = [None; ROOM_CAPACITY];
        let mut red_floors = [red::Floor::default(); ROOM_CAPACITY];
        let mut offsets = [(0_u16, 0_u32); ROOM_CAPACITY];
        let mut last = [(0_u16, 0_u32); ROOM_CAPACITY];
        let mut retry_after = [None; ROOM_CAPACITY];
        while let Some(Queued { delivery, _bytes }) = rx.recv().await {
            let Delivery {
                epoch,
                receiver_epoch,
                source_access,
                slot,
                source,
                source_active,
                mut packet,
                queued_at,
            } = delivery;
            if !self.active.load(Ordering::Acquire) {
                break;
            }
            let (packet_kind, source_slot) = MediaKind::from_track(slot).expect("delivery track");
            debug_assert_eq!(kind, packet_kind);
            let source_epoch = packet.header.csrc.first().copied();
            let packet_epoch = packet
                .header
                .csrc
                .get(if kind == MediaKind::Microphone { 1 } else { 0 })
                .copied();
            let screen_epoch = packet_epoch.unwrap_or(0);
            let receiver_microphone_epoch = if kind == MediaKind::Microphone {
                let members = self.room.members.read().await;
                let Some(publisher) = members.get(&source_slot) else {
                    continue;
                };
                let Some(receiver) = members.get(&self.receiver_slot) else {
                    continue;
                };
                if publisher.source_id != source
                    || packet_epoch != Some(publisher.info.microphone_epoch)
                    || source_epoch != Some(publisher.info.microphone_source_epoch)
                    || queued_at < receiver.red.receiver_started
                    || !viewing::receives(receiver, publisher, kind, queued_at)
                    || !red::receives(publisher, receiver)
                    || !red_floors[source_slot].prepare(
                        &mut packet,
                        source,
                        publisher.info.microphone_epoch,
                        receiver.red.receiver_floor,
                        receiver.packet_redundancy,
                    )
                {
                    continue;
                }
                // A globally ordered effective privacy revision also fences
                // ciphertext already sent before this listener's deafen boundary.
                // Keep packet_epoch above as the ORIGINAL source revision for
                // final source authorization; never compare this maximum to it.
                if publisher.packet_redundancy {
                    packet.header.csrc[1] = publisher
                        .info
                        .microphone_epoch
                        .max(receiver.red.receiver_floor);
                } else {
                    // Old publishers do not acknowledge capture generations:
                    // never advertise strict privacy/DRED support for them.
                    packet.header.csrc.clear();
                }
                Some(receiver.red.receiver_floor)
            } else {
                None
            };
            if sources[source_slot] != Some(source) {
                sources[source_slot] = Some(source);
                retry_after[source_slot] = None;
                offsets[source_slot] = (
                    last[source_slot]
                        .0
                        .wrapping_add(1)
                        .wrapping_sub(packet.header.sequence_number),
                    last[source_slot]
                        .1
                        .wrapping_add(if kind.is_video() { 9000 } else { 960 })
                        .wrapping_sub(packet.header.timestamp),
                );
            }
            let deadline = queued_at + max_age(kind);
            if Instant::now() >= deadline
                || retry_after[source_slot].is_some_and(|at| queued_at < at)
            {
                self.dropped(kind, source_slot, screen_epoch).await;
                continue;
            }
            // Preserve SRTP sequence/timestamp continuity on slot reuse, including
            // gaps and reordered packets within each publisher stream.
            packet.header.sequence_number = packet
                .header
                .sequence_number
                .wrapping_add(offsets[source_slot].0);
            packet.header.timestamp = packet.header.timestamp.wrapping_add(offsets[source_slot].1);
            if packet
                .header
                .sequence_number
                .wrapping_sub(last[source_slot].0) as i16
                > 0
            {
                last[source_slot] = (packet.header.sequence_number, packet.header.timestamp);
            }
            let delivered = tokio::time::timeout_at(
                deadline.into(),
                authorized_write(
                    &self.room,
                    || {
                        delivery_permits(
                            &source_access,
                            epoch,
                            &self.access,
                            receiver_epoch,
                            self.generation.load(Ordering::Acquire),
                        )
                    },
                    |members| {
                        Instant::now() < deadline
                            && self.active.load(Ordering::Acquire)
                            && source_active.load(Ordering::Acquire)
                            && members.get(&source_slot).is_some_and(|m| {
                                m.source_id == source
                                    && (kind != MediaKind::Microphone
                                        || source_epoch == Some(m.info.microphone_source_epoch))
                                    && packet_epoch
                                        == Some(if kind == MediaKind::Microphone {
                                            m.info.microphone_epoch
                                        } else {
                                            m.info.video_epoch(kind)
                                        })
                                    && members.get(&self.receiver_slot).is_some_and(|receiver| {
                                        (kind != MediaKind::Microphone
                                            || (receiver_microphone_epoch
                                                == Some(receiver.red.receiver_floor)
                                                && queued_at >= receiver.red.receiver_started))
                                            && viewing::receives(receiver, m, kind, queued_at)
                                            && (kind != MediaKind::Microphone
                                                || red::receives(m, receiver))
                                    })
                            })
                    },
                    write_media(kind, send(slot, packet)),
                ),
            )
            .await;
            // The deadline includes queue age and room/driver contention. An
            // expired microphone packet says nothing about transport health;
            // discard it and allow the next fresh packet to make progress.
            // Explicit transport errors below still end the voice connection.
            if kind == MediaKind::Microphone && delivered.is_err() {
                continue;
            }
            if matches!(delivered, Ok(Err(()))) {
                if Instant::now() >= deadline {
                    self.dropped(kind, source_slot, screen_epoch).await;
                }
                continue;
            }
            if !matches!(delivered, Ok(Ok(Ok(())))) {
                if kind == MediaKind::Microphone {
                    self.active.store(false, Ordering::Release);
                    break;
                }
                // Discard this source's backlog and briefly back off admission to
                // the shared driver. Other sources and microphone keep running.
                retry_after[source_slot] = Some(Instant::now() + SCREEN_BACKOFF);
                if kind.is_video() && delivered.is_err() {
                    self.metrics[4].fetch_add(1, Ordering::Relaxed);
                }
                self.dropped(kind, source_slot, screen_epoch).await;
                continue;
            }
            if kind.is_video() {
                self.metrics[3].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    async fn dropped(&self, kind: MediaKind, source_slot: usize, epoch: u32) {
        if kind.is_video() {
            self.metrics[2].fetch_add(1, Ordering::Relaxed);
            request_keyframe(
                &self.room,
                self.receiver_slot,
                source_slot,
                kind,
                epoch,
                &self.active,
            )
            .await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Clone, Copy, Debug)]
    enum ViewerChange {
        Unsubscribe,
        Expire,
        Restart,
    }

    async fn subscribe(room: &Room) {
        let publisher = room.members.read().await[&1].info.clone();
        assert!(
            viewing::update(
                room,
                0,
                vec![thiscord_shared::screen::Subscription {
                    kind: thiscord_shared::voice::MediaKind::ScreenVideo,
                    slot: 1,
                    owner: publisher.account_id,
                    epoch: publisher.screen_epoch,
                    bitrate: 2_500_000,
                }],
                &AtomicBool::new(true)
            )
            .await
        );
    }

    async fn change_viewer(room: &Room, change: ViewerChange) {
        match change {
            ViewerChange::Unsubscribe => {
                assert!(viewing::update(room, 0, vec![], &AtomicBool::new(true)).await);
            }
            ViewerChange::Expire => {
                room.members.write().await.get_mut(&0).unwrap().views[1]
                    .as_mut()
                    .unwrap()
                    .renewed -= Duration::from_secs(thiscord_shared::screen::VIEW_LEASE_SECS);
            }
            ViewerChange::Restart => {
                update_screen_state(room, 1, false, false, &AtomicBool::new(true))
                    .await
                    .unwrap();
                update_screen_state(room, 1, true, true, &AtomicBool::new(true))
                    .await
                    .unwrap();
                // A new viewer of the new share must never receive the old share's packets.
                subscribe(room).await;
            }
        }
    }

    async fn forward(room: &Room, tx: &Sender, packet: Delivery) {
        let members = room.members.read().await;
        tx.forward(&members[&1], &members[&0], packet).unwrap();
    }

    #[tokio::test]
    async fn queue_admission_filters_unsubscribed_expired_and_restarted_views() {
        for kind in [
            MediaKind::Microphone,
            MediaKind::ScreenVideo,
            MediaKind::SystemAudio,
        ] {
            for change in [
                ViewerChange::Unsubscribe,
                ViewerChange::Expire,
                ViewerChange::Restart,
            ] {
                let (writer, source, _) = fixture().await;
                let (tx, mut rx) = channel(kind);
                let budget = tx.bytes.available_permits();
                forward(&writer.room, &tx, delivery(kind, &source, 100)).await;
                drop(rx.try_recv().expect("active viewer must be admitted"));
                assert_eq!(tx.bytes.available_permits(), budget);
                change_viewer(&writer.room, change).await;
                // Timestamp after the mutation: restart must check the wire epoch,
                // not just whether there is now a viewer for this slot.
                forward(&writer.room, &tx, delivery(kind, &source, 100)).await;
                if kind == MediaKind::Microphone {
                    drop(rx.try_recv().expect("microphone must remain independent"));
                } else {
                    assert!(rx.try_recv().is_err(), "admitted {kind:?} after {change:?}");
                }
                assert_eq!(
                    tx.bytes.available_permits(),
                    budget,
                    "filtered packets reserved bytes"
                );
                subscribe(&writer.room).await;
                let mut fresh = delivery(kind, &source, 100);
                let info = writer.room.members.read().await[&1].info.clone();
                fresh.packet.header.csrc = if kind == MediaKind::Microphone {
                    vec![info.microphone_source_epoch, info.microphone_epoch]
                } else {
                    vec![info.screen_epoch]
                };
                forward(&writer.room, &tx, fresh).await;
                drop(
                    rx.try_recv()
                        .expect("fresh subscription must resume admission"),
                );
                assert_eq!(tx.bytes.available_permits(), budget);
            }
        }
    }

    #[tokio::test]
    async fn each_writer_rechecks_viewer_and_share_after_waiting_for_room_access() {
        for kind in [
            MediaKind::Microphone,
            MediaKind::ScreenVideo,
            MediaKind::SystemAudio,
        ] {
            for change in [
                ViewerChange::Unsubscribe,
                ViewerChange::Expire,
                ViewerChange::Restart,
            ] {
                let (writer, source, mut feedback) = fixture().await;
                let room = writer.room.clone();
                let active = writer.active.clone();
                let (tx, rx) = channel(kind);
                let budget = tx.bytes.available_permits();
                forward(&room, &tx, delivery(kind, &source, 100)).await;
                let (sent, mut received) = mpsc::unbounded_channel();
                let mut run = std::pin::pin!(tokio::task::unconstrained(writer.run(
                    kind,
                    rx,
                    move |_, packet| {
                        let sent = sent.clone();
                        async move { sent.send(packet).map_err(|_| ()) }
                    }
                )));
                // Deterministically stop between dequeue and the transport poll;
                // real elapsed time remains below the media's 100/150 ms deadline.
                let mut lock = room.members.write().await;
                assert!(futures_util::poll!(run.as_mut()).is_pending());
                assert_eq!(
                    tx.tx.capacity(),
                    tx.tx.max_capacity(),
                    "packet was not dequeued"
                );
                assert!(
                    tx.bytes.available_permits() < budget,
                    "in-flight reservation missing"
                );
                // Commit while owning the write lock: a queued read has priority
                // over a later writer, so it must observe this intervening change.
                match change {
                    ViewerChange::Unsubscribe => {
                        lock.get_mut(&0).unwrap().views = [None; ROOM_CAPACITY * 2]
                    }
                    ViewerChange::Expire => {
                        lock.get_mut(&0).unwrap().views[1].as_mut().unwrap().renewed -=
                            Duration::from_secs(thiscord_shared::screen::VIEW_LEASE_SECS);
                    }
                    ViewerChange::Restart => {
                        let publisher = lock.get_mut(&1).unwrap();
                        publisher.info.screen_epoch += 1;
                        publisher.screen_started = Instant::now();
                        let epoch = publisher.info.screen_epoch;
                        let replacement = lock.get_mut(&0).unwrap().views[1].as_mut().unwrap();
                        replacement.subscription.epoch = epoch;
                        replacement.since = Instant::now();
                        replacement.renewed = replacement.since;
                    }
                }
                drop(lock);
                assert!(futures_util::poll!(run.as_mut()).is_pending());
                if kind == MediaKind::Microphone {
                    received
                        .try_recv()
                        .expect("viewer changes must not stop microphone");
                } else {
                    assert!(
                        received.try_recv().is_err(),
                        "sent {kind:?} after {change:?}"
                    );
                }
                assert_eq!(tx.bytes.available_permits(), budget);
                assert!(
                    feedback.try_recv().is_err(),
                    "invalid viewer requested recovery"
                );

                if matches!(change, ViewerChange::Restart) && kind != MediaKind::Microphone {
                    // Bypass admission to exercise the final transport epoch check
                    // with a new queue timestamp and a valid replacement viewer.
                    tx.try_send(delivery(kind, &source, 100)).unwrap();
                    assert!(futures_util::poll!(run.as_mut()).is_pending());
                    assert!(
                        received.try_recv().is_err(),
                        "old share epoch reached transport"
                    );
                    assert_eq!(tx.bytes.available_permits(), budget);
                }
                subscribe(&room).await;
                let mut fresh = delivery(kind, &source, 100);
                let info = room.members.read().await[&1].info.clone();
                let epoch = if kind == MediaKind::Microphone {
                    info.microphone_epoch
                } else {
                    info.screen_epoch
                };
                let csrc = if kind == MediaKind::Microphone {
                    vec![info.microphone_source_epoch, epoch]
                } else {
                    vec![epoch]
                };
                fresh.packet.header.csrc = csrc.clone();
                forward(&room, &tx, fresh).await;
                drop(tx);
                run.await;
                assert_eq!(received.try_recv().unwrap().header.csrc, csrc);
                assert!(received.try_recv().is_err());
                assert!(active.load(Ordering::Acquire));
            }
        }
    }

    struct Source {
        id: uuid::Uuid,
        identity: access::Identity,
        access: Arc<access::Access>,
        generation: Arc<AtomicU64>,
    }
    fn source() -> Source {
        let identity = access::Identity {
            account: thiscord_shared::AccountId::from_uuid(uuid::Uuid::new_v4()),
            session: thiscord_shared::SessionId::from_uuid(uuid::Uuid::new_v4()),
            guild: None,
        };
        Source {
            id: uuid::Uuid::new_v4(),
            identity,
            access: access::register(identity),
            generation: Arc::new(AtomicU64::new(0)),
        }
    }
    fn delivery(kind: MediaKind, source: &Source, size: usize) -> Delivery {
        Delivery {
            epoch: source.generation.load(Ordering::Acquire),
            receiver_epoch: 0,
            source_access: source.access.clone(),
            slot: kind.track_index(1),
            source: source.id,
            source_active: Arc::new(AtomicBool::new(true)),
            packet: rtp::Packet {
                header: rtp::header::Header {
                    version: 2,
                    csrc: if kind == MediaKind::Microphone {
                        vec![7, 7]
                    } else {
                        vec![7]
                    },
                    payload_type: kind.payload_type(),
                    sequence_number: 100,
                    timestamp: 1234,
                    ..Default::default()
                },
                payload: vec![0; size].into(),
            },
            queued_at: Instant::now(),
        }
    }

    async fn fixture() -> (Writer, Source, mpsc::Receiver<(MediaKind, u32)>) {
        let room = Arc::new(Room::default());
        let source = source();
        let receiver = self::source();
        let active = Arc::new(AtomicBool::new(true));
        let generation = receiver.generation.clone();
        let metrics = Arc::new(std::array::from_fn(|_| AtomicU64::new(0)));
        let (keyframes, rx) = mpsc::channel(1);
        for slot in 0..2 {
            let endpoint = if slot == 0 { &receiver } else { &source };
            room.members.write().await.insert(
                slot,
                Member {
                    resumption: None,
                    request_id: thiscord_shared::RequestId::from_uuid(uuid::Uuid::new_v4()),
                    call_id: None,
                    replaced: watch::channel(false).0,
                    network: Arc::new(RwLock::new(Default::default())),
                    info: Participant {
                        account_id: endpoint.identity.account,
                        username: "test".into(),
                        display_name: String::new(),
                        avatar_id: None,
                        slot,
                        muted: false,
                        deafened: false,
                        can_speak: true,
                        sharing_screen: true,
                        sharing_audio: true,
                        screen_epoch: 7,
                        microphone_epoch: 7,
                        microphone_source_epoch: 7,
                        camera_active: false,
                        camera_epoch: 0,
                        screen_viewers: vec![],
                        camera_viewers: vec![],
                    },
                    source_id: endpoint.id,
                    screen_started: Instant::now(),
                    microphone_started: Instant::now() - Duration::from_secs(1),
                    negotiated: true,
                    packet_redundancy: slot == 1,
                    expected_sender_generation: None,
                    restricted_sender_generation: None,
                    red: red::Barrier::with_receiver_started(
                        Instant::now() - Duration::from_secs(1),
                    ),
                    camera_started: Instant::now(),
                    outgoing: MediaKind::ALL.map(|kind| channel(kind).0),
                    keyframes: keyframes.clone(),
                    last_keyframe: [None; 2],
                    feedback_enabled: true,
                    subscriptions_enabled: true,
                    views: [None; ROOM_CAPACITY * 2],
                    generation: endpoint.generation.clone(),
                    access: endpoint.access.clone(),
                    active: active.clone(),
                    metrics: metrics.clone(),
                },
            );
        }
        let owner = room.members.read().await[&1].info.account_id;
        assert!(
            viewing::update(
                &room,
                0,
                vec![thiscord_shared::screen::Subscription {
                    kind: thiscord_shared::voice::MediaKind::ScreenVideo,
                    slot: 1,
                    owner,
                    epoch: 7,
                    bitrate: 2_500_000,
                }],
                &AtomicBool::new(true)
            )
            .await
        );
        (
            Writer {
                access: receiver.access,
                room,
                receiver_slot: 0,
                active,
                generation,
                metrics,
            },
            source,
            rx,
        )
    }

    fn clone_writer(writer: &Writer) -> Writer {
        Writer {
            access: writer.access.clone(),
            room: writer.room.clone(),
            receiver_slot: writer.receiver_slot,
            active: writer.active.clone(),
            generation: writer.generation.clone(),
            metrics: writer.metrics.clone(),
        }
    }

    #[tokio::test]
    async fn microphone_red_fallback_and_remap_preserve_offsets() {
        use thiscord_shared::rtp_red as wire;
        for negotiated in [false, true] {
            let (writer, source, _) = fixture().await;
            writer
                .room
                .members
                .write()
                .await
                .get_mut(&0)
                .unwrap()
                .packet_redundancy = negotiated;
            let (tx, rx) = channel(MediaKind::Microphone);
            let (sent, mut received) = mpsc::channel(4);
            let task = tokio::spawn(writer.run(MediaKind::Microphone, rx, move |_, p| {
                let sent = sent.clone();
                async move { sent.send(p).await.map_err(|_| ()) }
            }));
            for n in 0..3u32 {
                let mut d = delivery(MediaKind::Microphone, &source, 0);
                d.packet.header.payload_type = wire::PAYLOAD_TYPE;
                d.packet.header.timestamp = (u32::MAX - 959).wrapping_add(n * 960);
                d.packet.header.sequence_number = 65535u16.wrapping_add(n as u16);
                d.packet.payload = wire::encode(
                    &[0x78, 3, 4],
                    &[
                        wire::Block {
                            offset: 1920,
                            payload: &[0x78, 1, 2],
                        },
                        wire::Block {
                            offset: 960,
                            payload: &[0x78, 2, 3],
                        },
                    ],
                )
                .unwrap()
                .into();
                tx.try_send(d).unwrap();
            }
            drop(tx);
            task.await.unwrap();
            for n in 0..3u32 {
                let p = received.recv().await.unwrap();
                assert_eq!(p.header.sequence_number, 1 + n as u16);
                assert_eq!(p.header.timestamp, 960 + n * 960);
                assert_eq!(p.header.csrc, vec![7, 7]);
                if negotiated {
                    assert_eq!(p.header.payload_type, wire::PAYLOAD_TYPE);
                    let red = wire::parse(&p.payload).unwrap();
                    assert_eq!(red.history.len(), n as usize);
                    for h in red.history {
                        assert!(p.header.timestamp - u32::from(h.offset) >= 960);
                    }
                } else {
                    assert_eq!(p.header.payload_type, wire::OPUS_PAYLOAD_TYPE);
                    assert_eq!(&p.payload[..], &[0x78, 3, 4]);
                }
            }
        }
    }

    #[tokio::test]
    async fn microphone_queue_cannot_survive_mute_or_deafen_cycle() {
        for affected_slot in [0, 1] {
            let (writer, source, _) = fixture().await;
            let stale = delivery(MediaKind::Microphone, &source, 3);
            update_voice_state(
                &writer.room,
                affected_slot,
                affected_slot == 1,
                affected_slot == 0,
                &writer.active,
            )
            .await;
            update_voice_state(&writer.room, affected_slot, false, false, &writer.active).await;
            let (tx, rx) = channel(MediaKind::Microphone);
            tx.try_send(stale).unwrap();
            let mut fresh = delivery(MediaKind::Microphone, &source, 3);
            let info = writer.room.members.read().await[&1].info.clone();
            fresh.packet.header.csrc = vec![info.microphone_source_epoch, info.microphone_epoch];
            tx.try_send(fresh).unwrap();
            drop(tx);
            let (sent, mut received) = mpsc::channel(4);
            writer
                .run(MediaKind::Microphone, rx, move |_, p| {
                    let sent = sent.clone();
                    async move { sent.send(p).await.map_err(|_| ()) }
                })
                .await;
            assert!(
                received.recv().await.is_some(),
                "fresh microphone packet rejected"
            );
            assert!(
                received.recv().await.is_none(),
                "pre-boundary packet survived"
            );
        }
    }

    #[tokio::test]
    async fn microphone_boundary_is_monotonic_idempotent_and_generation_tagged() {
        let (writer, source, _) = fixture().await;
        writer.room.microphone_epoch.store(7, Ordering::Relaxed);
        let mut room_changes = writer.room.changed.subscribe();
        let mut members = writer.room.members.write().await;
        let member = members.get_mut(&1).unwrap();
        let stable = member.info.microphone_source_epoch;
        member.packet_redundancy = false;
        assert!(
            !red::accept_boundary(&writer.room, member, 1),
            "legacy signaling cannot enable generations"
        );
        member.packet_redundancy = true;
        assert!(
            !red::accept_boundary(&writer.room, member, 0),
            "zero is reserved"
        );
        let mut p = delivery(MediaKind::Microphone, &source, 3).packet;
        p.header.csrc = vec![0, 1];
        assert!(
            !red::sender_generation(&p, member),
            "unacknowledged generation accepted"
        );
        assert!(red::accept_boundary(&writer.room, member, 1));
        assert!(
            room_changes.has_changed().unwrap(),
            "privacy update must wake roster listeners"
        );
        room_changes.borrow_and_update();
        assert_eq!(member.info.microphone_epoch, 8);
        assert_eq!(member.info.microphone_source_epoch, stable);
        assert!(red::sender_generation(&p, member));
        assert!(red::accept_boundary(&writer.room, member, 1));
        assert!(
            !room_changes.has_changed().unwrap(),
            "duplicate boundary caused a roster flood"
        );
        assert_eq!(member.info.microphone_epoch, 8, "duplicate rotated epoch");
        assert!(red::accept_boundary(&writer.room, member, 2));
        assert!(!red::sender_generation(&p, member));
        assert!(!red::accept_boundary(&writer.room, member, 1));
        p.header.csrc = vec![0, 2];
        assert!(red::sender_generation(&p, member));
        p.header.csrc.push(3);
        assert!(
            !red::sender_generation(&p, member),
            "noncanonical tag accepted"
        );
        writer
            .room
            .microphone_epoch
            .store(u32::MAX, Ordering::Relaxed);
        assert!(
            !red::accept_boundary(&writer.room, member, 3),
            "epoch overflow must fail closed"
        );
        assert!(!member.active.load(Ordering::Acquire));
        assert_eq!(member.info.microphone_epoch, 9);
        assert_eq!(red::next_epoch(&writer.room), None);
    }

    #[tokio::test]
    async fn modern_unmute_requires_a_new_acknowledged_sender_generation() {
        let (writer, _, _) = fixture().await;
        {
            let mut members = writer.room.members.write().await;
            let member = members.get_mut(&1).unwrap();
            member.packet_redundancy = true;
            assert!(red::accept_boundary(&writer.room, member, 1));
        }
        assert!(update_voice_state(&writer.room, 1, true, false, &writer.active).await);
        assert!(!update_voice_state(&writer.room, 1, false, false, &writer.active).await);
        assert!(writer.room.members.read().await[&1].info.muted);
        {
            let mut members = writer.room.members.write().await;
            assert!(red::accept_boundary(
                &writer.room,
                members.get_mut(&1).unwrap(),
                2
            ));
        }
        assert!(update_voice_state(&writer.room, 1, false, false, &writer.active).await);
        assert!(!writer.room.members.read().await[&1].info.muted);
        assert!(
            update_voice_state(&writer.room, 1, false, false, &writer.active).await,
            "unchanged state is idempotent"
        );
    }

    #[tokio::test]
    async fn microphone_effective_revision_includes_listener_privacy_boundary() {
        let (writer, source, _) = fixture().await;
        let mut changes = writer.room.changed.subscribe();
        writer.room.microphone_epoch.store(10, Ordering::Relaxed);
        let stale = delivery(MediaKind::Microphone, &source, 3);
        {
            let mut members = writer.room.members.write().await;
            assert!(red::accept_boundary(
                &writer.room,
                members.get_mut(&1).unwrap(),
                1
            ));
            let receiver = members.get_mut(&0).unwrap();
            receiver.packet_redundancy = true;
            assert!(red::accept_boundary(&writer.room, receiver, 1));
            red::challenge_publishers(&mut members, 0);
            assert!(
                !red::receives(&members[&1], &members[&0]),
                "old source was relabeled across receiver barrier"
            );
            let publisher = members.get_mut(&1).unwrap();
            let challenge = publisher.red.request(Instant::now()).unwrap();
            assert!(red::accept_refresh(&writer.room, publisher, 2, challenge));
        }
        assert!(changes.has_changed().unwrap());
        changes.borrow_and_update();
        let mut fresh = delivery(MediaKind::Microphone, &source, 3);
        fresh.packet.header.csrc[1] = writer.room.members.read().await[&1].info.microphone_epoch;
        let (tx, rx) = channel(MediaKind::Microphone);
        tx.try_send(stale).unwrap();
        tx.try_send(fresh).unwrap();
        drop(tx);
        let (sent, mut received) = mpsc::channel(4);
        writer
            .run(MediaKind::Microphone, rx, move |_, packet| {
                let sent = sent.clone();
                async move { sent.send(packet).await.map_err(|_| ()) }
            })
            .await;
        let packet = received
            .recv()
            .await
            .expect("new listener revision must permit fresh media");
        assert_eq!(packet.header.csrc, vec![7, 13]);
        assert!(
            received.recv().await.is_none(),
            "queued pre-listener-boundary packet survived"
        );
    }

    #[tokio::test]
    async fn legacy_publishers_never_advertise_strict_privacy_or_dred_support() {
        let (writer, source, _) = fixture().await;
        {
            let mut members = writer.room.members.write().await;
            members.get_mut(&0).unwrap().packet_redundancy = true;
            let publisher = members.get_mut(&1).unwrap();
            publisher.packet_redundancy = false;
            let visible = red::participant(publisher);
            assert_eq!(visible.microphone_epoch, 0);
            assert_eq!(visible.microphone_source_epoch, 0);
            assert_ne!(
                publisher.info.microphone_epoch, 0,
                "internal fence was erased"
            );
        }
        let (tx, rx) = channel(MediaKind::Microphone);
        tx.try_send(delivery(MediaKind::Microphone, &source, 3))
            .unwrap();
        drop(tx);
        let (sent, mut received) = mpsc::channel(1);
        writer
            .run(MediaKind::Microphone, rx, move |_, packet| {
                let sent = sent.clone();
                async move { sent.send(packet).await.map_err(|_| ()) }
            })
            .await;
        let packet = received.recv().await.unwrap();
        assert!(packet.header.csrc.is_empty());
        assert_eq!(
            packet.header.payload_type,
            thiscord_shared::rtp_red::OPUS_PAYLOAD_TYPE
        );
    }

    #[tokio::test]
    async fn challenge_proof_blocks_delayed_ingress_and_preissued_generic_generations() {
        let (writer, source, _) = fixture().await;
        writer.room.microphone_epoch.store(7, Ordering::Relaxed);
        let mut members = writer.room.members.write().await;
        members.get_mut(&0).unwrap().packet_redundancy = true;
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            1
        ));
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&0).unwrap(),
            1
        ));
        red::challenge_publishers(&mut members, 0);
        let challenge = members
            .get_mut(&1)
            .unwrap()
            .red
            .request(Instant::now())
            .unwrap();
        let mut held = delivery(MediaKind::Microphone, &source, 3).packet;
        held.header.csrc = vec![0, 1];
        assert!(red::sender_generation(&held, &members[&1]));
        assert!(
            !red::receives(&members[&1], &members[&0]),
            "delayed old ingress was relabeled"
        );
        // This generic request may have been sent before the listener barrier
        // and delayed independently. Numeric freshness is not causal proof.
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            10
        ));
        held.header.csrc = vec![0, 10];
        assert!(red::sender_generation(&held, &members[&1]));
        assert!(!red::receives(&members[&1], &members[&0]));
        assert!(!red::accept_refresh(
            &writer.room,
            members.get_mut(&1).unwrap(),
            10,
            challenge
        ));
        let receiver_floor = members[&1].red.receiver_floor;
        let other_pending = members[&0].red.pending;
        assert!(red::accept_refresh(
            &writer.room,
            members.get_mut(&1).unwrap(),
            11,
            challenge
        ));
        assert_eq!(
            members[&1].red.receiver_floor, receiver_floor,
            "source refresh moved receive barrier"
        );
        assert_eq!(
            members[&0].red.pending, other_pending,
            "refresh recursively challenged another peer"
        );
        assert!(red::receives(&members[&1], &members[&0]));
        assert!(
            !red::sender_generation(&held, &members[&1]),
            "held prechallenge UDP survived proof acknowledgement"
        );
        held.header.csrc = vec![0, 11];
        assert!(red::sender_generation(&held, &members[&1]));
        assert!(
            !red::accept_boundary(&writer.room, members.get_mut(&1).unwrap(), 11),
            "source-only ACK was confused with full barrier ACK"
        );
    }

    #[tokio::test]
    async fn challenges_coalesce_concurrent_receivers_and_bind_publisher_membership() {
        let (writer, _, _) = fixture().await;
        writer.room.microphone_epoch.store(7, Ordering::Relaxed);
        let mut members = writer.room.members.write().await;
        let mut second = members[&0].clone();
        second.source_id = uuid::Uuid::new_v4();
        second.info.slot = 2;
        second.packet_redundancy = true;
        second.red = red::Barrier::default();
        members.insert(2, second);
        members.get_mut(&0).unwrap().packet_redundancy = true;
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            1
        ));
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&0).unwrap(),
            1
        ));
        red::challenge_publishers(&mut members, 0);
        let now = Instant::now();
        let first = members.get_mut(&1).unwrap().red.request(now).unwrap();
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&2).unwrap(),
            1
        ));
        red::challenge_publishers(&mut members, 2);
        let latest = members[&2].red.receiver_floor;
        let before = members[&1].info.microphone_epoch;
        assert!(latest > first);
        assert!(
            !red::accept_refresh(&writer.room, members.get_mut(&1).unwrap(), 2, latest),
            "unissued challenge accepted"
        );
        assert_eq!(members[&1].info.microphone_epoch, before);
        // Current ISSUED challenge remains valid despite newer pending floors.
        assert!(red::accept_refresh_at(
            &writer.room,
            members.get_mut(&1).unwrap(),
            2,
            first,
            now
        ));
        assert!(red::ready(members.get_mut(&1).unwrap(), 2, first, now));
        assert!(members[&1].info.microphone_epoch > latest);
        assert!(red::receives(&members[&1], &members[&0]));
        assert!(
            !red::receives(&members[&1], &members[&2]),
            "new high source revision laundered unproven receiver barrier"
        );
        assert_eq!(
            members.get_mut(&1).unwrap().red.request(now),
            None,
            "ACK bypassed issue cadence"
        );
        assert_eq!(
            members
                .get_mut(&1)
                .unwrap()
                .red
                .request(now + Duration::from_millis(250)),
            Some(latest)
        );
        assert!(
            !red::accept_refresh(&writer.room, members.get_mut(&1).unwrap(), 3, first),
            "stale issued challenge accepted"
        );
        assert!(red::accept_refresh(
            &writer.room,
            members.get_mut(&1).unwrap(),
            3,
            latest
        ));
        assert!(red::receives(&members[&1], &members[&0]));
        assert!(red::receives(&members[&1], &members[&2]));
        // Source replacement cannot inherit its predecessor's satisfaction or
        // outstanding request, even if its generation counter starts similarly.
        let source = members.get_mut(&1).unwrap();
        source.source_id = uuid::Uuid::new_v4();
        source.red = red::Barrier::default();
        source.expected_sender_generation = None;
        assert!(!red::accept_refresh(&writer.room, source, 2, latest));
        red::challenge_new_publisher(&mut members, 1);
        assert!(!red::receives(&members[&1], &members[&0]));
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            1
        ));
        assert_eq!(
            red::request(members.get_mut(&1).unwrap(), Instant::now()),
            Some(latest)
        );
        assert!(red::accept_refresh(
            &writer.room,
            members.get_mut(&1).unwrap(),
            2,
            latest
        ));
        assert!(red::receives(&members[&1], &members[&2]));
    }

    #[tokio::test]
    async fn delayed_ack_readiness_guarantees_actual_media_service_before_next_challenge() {
        let (writer, source, _) = fixture().await;
        writer.room.microphone_epoch.store(7, Ordering::Relaxed);
        let mut members = writer.room.members.write().await;
        members.get_mut(&0).unwrap().packet_redundancy = true;
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            1
        ));
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&0).unwrap(),
            1
        ));
        red::challenge_publishers(&mut members, 0);
        let now = Instant::now();
        let issued = red::request(members.get_mut(&1).unwrap(), now).unwrap();
        for elapsed in [250, 500] {
            assert_eq!(
                red::request(
                    members.get_mut(&1).unwrap(),
                    now + Duration::from_millis(elapsed)
                ),
                Some(issued)
            );
        }
        let mut churned = members[&0].clone();
        churned.info.slot = 2;
        members.insert(2, churned);
        for generation in 2..100 {
            assert!(red::accept_boundary(
                &writer.room,
                members.get_mut(&2).unwrap(),
                generation
            ));
            red::challenge_publishers(&mut members, 2);
        }
        let pending = members[&2].red.receiver_floor;
        assert!(red::accept_refresh_at(
            &writer.room,
            members.get_mut(&1).unwrap(),
            2,
            issued,
            now + Duration::from_secs(1)
        ));
        assert_eq!(
            red::request(
                members.get_mut(&1).unwrap(),
                now + Duration::from_millis(1500)
            ),
            None
        );
        assert!(!red::receives(&members[&1], &members[&2]));
        assert!(!red::ready(
            members.get_mut(&1).unwrap(),
            1,
            issued,
            now + Duration::from_secs(2)
        ));
        // A full PTT boundary can outrank the proof ACK before Ready is emitted.
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            3
        ));
        assert!(red::ready(
            members.get_mut(&1).unwrap(),
            3,
            issued,
            now + Duration::from_secs(2)
        ));
        let epoch = members[&1].info.microphone_epoch;
        assert!(red::ready(
            members.get_mut(&1).unwrap(),
            3,
            issued,
            now + Duration::from_millis(2200)
        ));
        assert_eq!(members[&1].info.microphone_epoch, epoch);
        assert_eq!(
            red::request(
                members.get_mut(&1).unwrap(),
                now + Duration::from_millis(2249)
            ),
            None
        );
        drop(members);
        let mut fresh = delivery(MediaKind::Microphone, &source, 3);
        fresh.packet.header.csrc[1] = epoch;
        let (tx, rx) = channel(MediaKind::Microphone);
        tx.try_send(fresh).unwrap();
        drop(tx);
        let (sent, mut received) = mpsc::channel(1);
        clone_writer(&writer)
            .run(MediaKind::Microphone, rx, move |_, packet| {
                let sent = sent.clone();
                async move { sent.send(packet).await.map_err(|_| ()) }
            })
            .await;
        assert_eq!(
            received
                .recv()
                .await
                .expect("eligible listener received fresh RTP during Ready cooldown")
                .header
                .csrc[1],
            epoch
        );
        let mut members = writer.room.members.write().await;
        assert_eq!(
            red::request(
                members.get_mut(&1).unwrap(),
                now + Duration::from_millis(2250)
            ),
            Some(pending)
        );
        assert!(red::receives(&members[&1], &members[&0]));
        assert!(!red::receives(&members[&1], &members[&2]));
    }

    #[tokio::test]
    async fn lost_proof_ack_reproof_after_full_boundary_preserves_deadline_and_cooldown() {
        let (writer, _, _) = fixture().await;
        let mut members = writer.room.members.write().await;
        members.get_mut(&0).unwrap().packet_redundancy = true;
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            1
        ));
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&0).unwrap(),
            1
        ));
        red::challenge_publishers(&mut members, 0);
        let now = Instant::now();
        let source = members.get_mut(&1).unwrap();
        let challenge = red::request(source, now).unwrap();
        assert!(red::accept_refresh_at(
            &writer.room,
            source,
            2,
            challenge,
            now + Duration::from_secs(1)
        ));
        // Generation 2 proof ACK is lost; a real local boundary supersedes it.
        assert!(red::accept_boundary(&writer.room, source, 3));
        let receiver_floor = source.red.receiver_floor;
        let epoch = source.info.microphone_epoch;
        assert!(red::accept_refresh_at(
            &writer.room,
            source,
            4,
            challenge,
            now + Duration::from_secs(4)
        ));
        assert!(source.info.microphone_epoch > epoch);
        assert_eq!(source.red.receiver_floor, receiver_floor);
        assert_eq!(source.expected_sender_generation, Some(4));
        assert!(!red::accept_refresh_at(
            &writer.room,
            source,
            3,
            challenge,
            now + Duration::from_secs(4)
        ));
        // Without Ready, neither fresh nor duplicate proof can extend the
        // original t=1+5 deadline. Use an isolated active flag for this branch.
        let mut missing_ready = source.clone();
        missing_ready.active = Arc::new(AtomicBool::new(true));
        assert!(!red::accept_refresh_at(
            &writer.room,
            &mut missing_ready,
            5,
            challenge,
            now + Duration::from_secs(6)
        ));
        assert!(!red::accept_refresh_at(
            &writer.room,
            &mut missing_ready,
            4,
            challenge,
            now + Duration::from_secs(6)
        ));
        assert!(red::expire(
            &writer.room,
            &missing_ready,
            now + Duration::from_secs(6)
        ));
        assert!(red::ready(
            source,
            4,
            challenge,
            now + Duration::from_secs(4)
        ));
        assert!(red::accept_refresh_at(
            &writer.room,
            source,
            5,
            challenge,
            now + Duration::from_millis(4200)
        ));
        assert!(red::ready(
            source,
            5,
            challenge,
            now + Duration::from_millis(4200)
        ));
        assert!(!red::expire(
            &writer.room,
            source,
            now + Duration::from_secs(10)
        ));
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&0).unwrap(),
            2
        ));
        red::challenge_publishers(&mut members, 0);
        let latest = members[&0].red.receiver_floor;
        assert_eq!(
            red::request(
                members.get_mut(&1).unwrap(),
                now + Duration::from_millis(4249)
            ),
            None
        );
        assert_eq!(
            red::request(
                members.get_mut(&1).unwrap(),
                now + Duration::from_millis(4250)
            ),
            Some(latest)
        );
        assert!(!red::accept_refresh_at(
            &writer.room,
            members.get_mut(&1).unwrap(),
            6,
            challenge,
            now + Duration::from_millis(4251)
        ));
    }

    #[tokio::test]
    async fn missing_ready_expires_without_duplicate_proofs_extending_deadline() {
        let (writer, _, _) = fixture().await;
        let mut members = writer.room.members.write().await;
        members.get_mut(&0).unwrap().packet_redundancy = true;
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            1
        ));
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&0).unwrap(),
            1
        ));
        red::challenge_publishers(&mut members, 0);
        let now = Instant::now();
        let source = members.get_mut(&1).unwrap();
        let challenge = red::request(source, now).unwrap();
        assert!(red::accept_refresh_at(
            &writer.room,
            source,
            2,
            challenge,
            now + Duration::from_secs(1)
        ));
        assert!(red::accept_refresh_at(
            &writer.room,
            source,
            2,
            challenge,
            now + Duration::from_secs(5)
        ));
        assert!(!red::expire(
            &writer.room,
            source,
            now + Duration::from_millis(5999)
        ));
        assert!(!red::ready(
            source,
            2,
            challenge,
            now + Duration::from_secs(6)
        ));
        assert!(red::expire(
            &writer.room,
            source,
            now + Duration::from_secs(6)
        ));
    }

    #[tokio::test]
    async fn microphone_challenge_timeout_closes_only_unresponsive_source_and_removes_roster() {
        let (writer, source, _) = fixture().await;
        writer.room.microphone_epoch.store(7, Ordering::Relaxed);
        let now = Instant::now();
        {
            let mut members = writer.room.members.write().await;
            members.get_mut(&0).unwrap().packet_redundancy = true;
            members.get_mut(&1).unwrap().active = Arc::new(AtomicBool::new(true));
            assert!(red::accept_boundary(
                &writer.room,
                members.get_mut(&0).unwrap(),
                1
            ));
            red::challenge_publishers(&mut members, 0);
            assert!(red::accept_boundary(
                &writer.room,
                members.get_mut(&1).unwrap(),
                1
            ));
            assert!(red::request(members.get_mut(&1).unwrap(), now).is_some());
            assert!(!red::expire(
                &writer.room,
                &members[&1],
                now + Duration::from_millis(4999)
            ));
            assert!(red::expire(
                &writer.room,
                &members[&1],
                now + Duration::from_secs(5)
            ));
            assert!(!members[&1].active.load(Ordering::Acquire));
            assert!(
                members[&0].active.load(Ordering::Acquire),
                "listener was disconnected by source timeout"
            );
            assert!(
                !red::expire(&writer.room, &members[&1], now + Duration::from_secs(6)),
                "timeout emitted twice"
            );
        }
        // Production's terminal branch sends a clear Error and follows this
        // same membership cleanup; unrelated membership remains in the room.
        release_member(&writer.room, 1, source.id).await;
        let members = writer.room.members.read().await;
        assert_eq!(members.len(), 1);
        assert!(members.contains_key(&0));
        assert!(
            !viewing::participants(&members)
                .iter()
                .any(|member| member.slot == 1)
        );
    }

    #[tokio::test]
    async fn microphone_startup_waits_for_first_full_boundary_before_challenge_deadline() {
        let (writer, _, _) = fixture().await;
        writer.room.microphone_epoch.store(7, Ordering::Relaxed);
        let mut members = writer.room.members.write().await;
        members.get_mut(&0).unwrap().packet_redundancy = true;
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&0).unwrap(),
            1
        ));
        red::challenge_publishers(&mut members, 0);
        let later = Instant::now() + Duration::from_secs(8);
        assert_eq!(red::request(members.get_mut(&1).unwrap(), later), None);
        assert!(!red::expire(&writer.room, &members[&1], later));
        assert!(members[&1].active.load(Ordering::Acquire));
        assert!(red::accept_boundary(
            &writer.room,
            members.get_mut(&1).unwrap(),
            1
        ));
        let challenge = red::request(members.get_mut(&1).unwrap(), later).unwrap();
        assert!(!red::expire(
            &writer.room,
            &members[&1],
            later + Duration::from_millis(4999)
        ));
        assert!(
            red::accept_refresh(&writer.room, members.get_mut(&1).unwrap(), 2, challenge),
            "startup/muted source can respond entirely over signaling"
        );
        assert!(red::ready(
            members.get_mut(&1).unwrap(),
            2,
            challenge,
            Instant::now()
        ));
        assert!(!red::expire(
            &writer.room,
            &members[&1],
            later + Duration::from_secs(5)
        ));
    }

    #[tokio::test]
    async fn challenge_reply_at_timeout_boundary_fails_closed_without_epoch_or_generation_mutation()
    {
        for elapsed in [Duration::from_millis(4999), Duration::from_secs(5)] {
            let (writer, _, _) = fixture().await;
            writer.room.microphone_epoch.store(7, Ordering::Relaxed);
            let mut members = writer.room.members.write().await;
            members.get_mut(&0).unwrap().packet_redundancy = true;
            assert!(red::accept_boundary(
                &writer.room,
                members.get_mut(&1).unwrap(),
                1
            ));
            assert!(red::accept_boundary(
                &writer.room,
                members.get_mut(&0).unwrap(),
                1
            ));
            red::challenge_publishers(&mut members, 0);
            let now = Instant::now();
            let source = members.get_mut(&1).unwrap();
            let challenge = red::request(source, now).unwrap();
            let previous = source.info.microphone_epoch;
            let accepted =
                red::accept_refresh_at(&writer.room, source, 2, challenge, now + elapsed);
            assert_eq!(accepted, elapsed < Duration::from_secs(5));
            if !accepted {
                assert_eq!(source.info.microphone_epoch, previous);
                assert_eq!(source.expected_sender_generation, Some(1));
                assert_eq!(source.red.satisfied, 0);
                assert!(red::expire(&writer.room, source, now + elapsed));
            } else {
                assert!(!red::expire(&writer.room, source, now + elapsed));
            }
        }
    }

    #[tokio::test]
    async fn stalled_video_releases_driver_waiter_keeps_voice_connected_and_recovers() {
        let (writer, source, mut feedback) = fixture().await;
        let active = writer.active.clone();
        let metrics = writer.metrics.clone();
        let microphone = clone_writer(&writer);
        let (video_tx, video_rx) = channel(MediaKind::ScreenVideo);
        let (mic_tx, mic_rx) = channel(MediaKind::Microphone);
        // Use the pinned WebRTC driver's actual queue primitive, initially full.
        let (driver, mut received) = webrtc::runtime::channel(1);
        driver.send(0).await.unwrap();
        let video_driver = driver.clone();
        let video = tokio::spawn(writer.run(MediaKind::ScreenVideo, video_rx, move |_, _| {
            let driver = video_driver.clone();
            async move { driver.send(2).await.map_err(|_| ()) }
        }));
        video_tx
            .try_send(delivery(MediaKind::ScreenVideo, &source, 1000))
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), feedback.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(active.load(Ordering::Acquire));
        assert_eq!(metrics[2].load(Ordering::Relaxed), 1);
        assert_eq!(
            metrics[4].load(Ordering::Relaxed),
            0,
            "backpressure is not a timeout"
        );

        let (entered, mut waiting) = mpsc::channel(1);
        let mic = tokio::spawn(microphone.run(MediaKind::Microphone, mic_rx, move |_, _| {
            let driver = driver.clone();
            let entered = entered.clone();
            async move {
                entered.try_send(()).unwrap();
                driver.send(1).await.map_err(|_| ())
            }
        }));
        mic_tx
            .try_send(delivery(MediaKind::Microphone, &source, 100))
            .unwrap();
        waiting.recv().await.unwrap();
        assert_eq!(received.recv().await, Some(0));
        assert_eq!(
            received.recv().await,
            Some(1),
            "a canceled video write resumed before microphone delivery"
        );
        assert!(active.load(Ordering::Acquire));

        tokio::time::sleep(SCREEN_BACKOFF + Duration::from_millis(5)).await;
        video_tx
            .try_send(delivery(MediaKind::ScreenVideo, &source, 1000))
            .unwrap();
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(1), received.recv())
                .await
                .unwrap(),
            Some(2)
        );
        drop(video_tx);
        drop(mic_tx);
        video.await.unwrap();
        mic.await.unwrap();
        assert!(active.load(Ordering::Acquire));
        assert_eq!(metrics[3].load(Ordering::Relaxed), 1);
    }

    #[tokio::test]
    async fn healthy_screen_bursts_do_not_confuse_cooperative_yields_with_backpressure() {
        let (tx, mut rx) = mpsc::channel(1);
        for n in 0..1024 {
            write_media(MediaKind::ScreenVideo, async {
                tx.send(n).await.map_err(|_| ())
            })
            .await
            .unwrap();
            assert_eq!(rx.try_recv().unwrap(), n);
        }
    }

    #[tokio::test]
    async fn screen_transport_errors_are_nonfatal_and_microphone_errors_still_disconnect() {
        for kind in [
            MediaKind::Microphone,
            MediaKind::ScreenVideo,
            MediaKind::SystemAudio,
        ] {
            let (writer, source, mut feedback) = fixture().await;
            let active = writer.active.clone();
            let (tx, rx) = channel(kind);
            tx.try_send(delivery(kind, &source, 100)).unwrap();
            drop(tx);
            writer.run(kind, rx, |_, _| async { Err(()) }).await;
            assert_eq!(
                active.load(Ordering::Acquire),
                kind != MediaKind::Microphone
            );
            if kind.is_video() {
                assert_eq!(feedback.try_recv().unwrap(), (MediaKind::ScreenVideo, 7));
            }
        }
    }

    #[tokio::test]
    async fn byte_packet_and_age_budgets_release_on_drop_close_and_cancel() {
        for kind in [
            MediaKind::Microphone,
            MediaKind::ScreenVideo,
            MediaKind::SystemAudio,
        ] {
            let source = source();
            let (tx, mut rx) = channel(kind);
            let budget = tx.bytes.available_permits();
            let mut stale = delivery(kind, &source, 1);
            stale.queued_at -= max_age(kind);
            assert!(tx.try_send(stale).is_err());
            assert!(tx.try_send(delivery(kind, &source, budget)).is_err());
            let mut accepted = 0;
            while tx.try_send(delivery(kind, &source, 1500)).is_ok() {
                accepted += 1;
            }
            assert!(accepted > 0);
            assert!(
                accepted < tx.tx.max_capacity(),
                "byte budget must bind before packet count"
            );
            let in_flight = rx.recv().await.unwrap();
            assert!(
                tx.try_send(delivery(kind, &source, 1500)).is_err(),
                "in-flight bytes unaccounted"
            );
            let task = tokio::spawn(async move {
                let _in_flight = in_flight;
                std::future::pending::<()>().await;
            });
            task.abort();
            let _ = task.await;
            tx.try_send(delivery(kind, &source, 1500)).unwrap();
            drop(rx);
            assert_eq!(tx.bytes.available_permits(), budget);
            assert!(tx.try_send(delivery(kind, &source, 1)).is_err());
            assert_eq!(tx.bytes.available_permits(), budget);

            let (tx, rx) = channel(kind);
            for _ in 0..tx.tx.max_capacity() {
                tx.try_send(delivery(kind, &source, 1)).unwrap();
            }
            assert!(tx.try_send(delivery(kind, &source, 1)).is_err());
            drop(rx);
            assert_eq!(tx.bytes.available_permits(), budget);
        }
    }

    #[tokio::test]
    async fn queued_video_expires_without_touching_transport_and_requests_recovery() {
        let (writer, source, mut feedback) = fixture().await;
        let active = writer.active.clone();
        let (tx, mut rx) = channel(MediaKind::ScreenVideo);
        tx.try_send(delivery(MediaKind::ScreenVideo, &source, 1000))
            .unwrap();
        let mut queued = rx.recv().await.unwrap();
        queued.delivery.queued_at -= VIDEO_AGE;
        tx.tx.try_send(queued).ok().unwrap();
        drop(tx);
        writer
            .run(MediaKind::ScreenVideo, rx, |_, _| async {
                panic!("expired packet sent")
            })
            .await;
        assert_eq!(feedback.try_recv().unwrap(), (MediaKind::ScreenVideo, 7));
        assert!(active.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn video_deadline_includes_waiting_for_room_access() {
        let (writer, source, mut feedback) = fixture().await;
        let room = writer.room.clone();
        let members = room.members.write().await;
        let (tx, rx) = channel(MediaKind::ScreenVideo);
        tx.try_send(delivery(MediaKind::ScreenVideo, &source, 1000))
            .unwrap();
        drop(tx);
        let task = tokio::spawn(writer.run(MediaKind::ScreenVideo, rx, |_, _| async {
            panic!("late packet sent")
        }));
        tokio::time::sleep(VIDEO_AGE + Duration::from_millis(10)).await;
        drop(members);
        task.await.unwrap();
        assert_eq!(feedback.try_recv().unwrap(), (MediaKind::ScreenVideo, 7));
    }

    #[tokio::test]
    async fn microphone_packet_expiry_keeps_membership_and_delivers_fresh_audio() {
        for wait_for_room in [true, false] {
            let (writer, source, _) = fixture().await;
            let active = writer.active.clone();
            let room = writer.room.clone();
            let members = if wait_for_room {
                Some(room.members.write().await)
            } else {
                None
            };
            let (tx, rx) = channel(MediaKind::Microphone);
            let mut packet = delivery(MediaKind::Microphone, &source, 100);
            packet.queued_at -= AUDIO_AGE / 2;
            tx.try_send(packet).unwrap();
            let writes = AtomicU64::new(0);
            let mut run = std::pin::pin!(writer.run(MediaKind::Microphone, rx, |_, packet| {
                let writes = &writes;
                async move {
                    if packet.header.sequence_number == 1 {
                        std::future::pending::<()>().await;
                    }
                    writes.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }
            }));
            // Start the write before expiry; merely dequeuing an expired packet
            // exercises a different, already nonfatal branch.
            assert!(
                std::future::poll_fn(|cx| Poll::Ready(run.as_mut().poll(cx)))
                    .await
                    .is_pending()
            );
            tokio::time::sleep(AUDIO_AGE).await;
            let expired = std::future::poll_fn(|cx| Poll::Ready(run.as_mut().poll(cx))).await;
            assert!(
                active.load(Ordering::Acquire),
                "packet expiry disconnected voice (room wait: {wait_for_room})"
            );
            assert!(expired.is_pending());
            drop(members);
            let mut fresh = delivery(MediaKind::Microphone, &source, 100);
            fresh.packet.header.sequence_number += 1;
            tx.try_send(fresh).unwrap();
            drop(tx);
            tokio::time::timeout(AUDIO_AGE, run).await.unwrap();
            assert_eq!(writes.load(Ordering::Relaxed), 1);
            assert!(active.load(Ordering::Acquire));
        }
    }

    #[tokio::test]
    async fn every_writer_rejects_either_endpoints_revoked_or_stale_queue() {
        for kind in [
            MediaKind::Microphone,
            MediaKind::ScreenVideo,
            MediaKind::SystemAudio,
        ] {
            for revoke_source in [true, false] {
                for reauthorize in [false, true] {
                    let (writer, source, _) = fixture().await;
                    let active = writer.active.clone();
                    let (tx, rx) = channel(kind);
                    // Queue under the old grants, before the DB mutation drains.
                    forward(&writer.room, &tx, delivery(kind, &source, 100)).await;
                    let receiver = writer.room.members.read().await[&0].info.account_id;
                    let scope = if revoke_source {
                        access::Scope::Session(source.identity.session)
                    } else {
                        access::Scope::Account(receiver)
                    };
                    let mut mutation = Some(access::pause(scope));
                    // Filter new arrivals while either endpoint is paused, without
                    // consuming queue budget or requesting congestion recovery.
                    let budget = tx.bytes.available_permits();
                    forward(&writer.room, &tx, delivery(kind, &source, 100)).await;
                    assert_eq!(tx.bytes.available_permits(), budget);
                    if reauthorize {
                        mutation.take().unwrap().commit();
                        source
                            .generation
                            .store(source.access.snapshot().unwrap(), Ordering::Release);
                        writer
                            .generation
                            .store(writer.access.snapshot().unwrap(), Ordering::Release);
                        // Only a newly admitted packet can use the new grants.
                        let mut fresh = delivery(kind, &source, 100);
                        fresh.receiver_epoch = writer.generation.load(Ordering::Acquire);
                        fresh.packet.header.sequence_number += 1;
                        assert_ne!(fresh.epoch, fresh.receiver_epoch);
                        forward(&writer.room, &tx, fresh).await;
                    }
                    drop(tx);
                    let writes = AtomicU64::new(0);
                    writer
                        .run(kind, rx, |_, _| async {
                            writes.fetch_add(1, Ordering::Relaxed);
                            Ok(())
                        })
                        .await;
                    assert_eq!(
                        writes.load(Ordering::Relaxed),
                        u64::from(reauthorize),
                        "revoked/stale packet reached transport: {kind:?}, source={revoke_source}"
                    );
                    assert!(
                        active.load(Ordering::Acquire),
                        "dropping revoked media must not fail the transport"
                    );
                    drop(mutation);
                }
            }
        }
    }

    #[tokio::test]
    async fn pending_microphone_write_rechecks_both_endpoints_after_revocation() {
        for revoke_source in [true, false] {
            let (writer, source, _) = fixture().await;
            let active = writer.active.clone();
            let receiver = writer.room.members.read().await[&0].info.account_id;
            let (tx, rx) = channel(MediaKind::Microphone);
            tx.try_send(delivery(MediaKind::Microphone, &source, 100))
                .unwrap();
            drop(tx);
            let (entered, mut waiting) = mpsc::channel(1);
            let resume = Arc::new(Notify::new());
            let ready = resume.clone();
            let writes = Arc::new(AtomicU64::new(0));
            let sent = writes.clone();
            let task = tokio::spawn(writer.run(MediaKind::Microphone, rx, move |_, _| {
                let entered = entered.clone();
                let ready = ready.clone();
                let sent = sent.clone();
                async move {
                    entered.try_send(()).unwrap();
                    ready.notified().await;
                    sent.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }
            }));
            waiting.recv().await.unwrap();
            let scope = if revoke_source {
                access::Scope::Session(source.identity.session)
            } else {
                access::Scope::Account(receiver)
            };
            tokio::time::timeout(
                Duration::from_secs(1),
                tokio::task::spawn_blocking(move || {
                    access::pause(scope).commit();
                }),
            )
            .await
            .expect("pending send retained a drain permit")
            .unwrap();
            resume.notify_one();
            task.await.unwrap();
            assert!(
                active.load(Ordering::Acquire),
                "write timed out instead of rejecting revocation"
            );
            assert_eq!(
                writes.load(Ordering::Relaxed),
                0,
                "pending revoked media reached transport"
            );
        }
    }

    #[tokio::test]
    async fn pending_microphone_write_rechecks_source_and_listener_privacy() {
        for changed_slot in [0, 1] {
            let (writer, source, _) = fixture().await;
            let room = writer.room.clone();
            room.microphone_epoch.store(7, Ordering::Relaxed);
            let active = writer.active.clone();
            let (tx, rx) = channel(MediaKind::Microphone);
            tx.try_send(delivery(MediaKind::Microphone, &source, 100))
                .unwrap();
            drop(tx);
            let (entered, mut waiting) = mpsc::channel(1);
            let resume = Arc::new(Notify::new());
            let ready = resume.clone();
            let writes = Arc::new(AtomicU64::new(0));
            let sent = writes.clone();
            let task = tokio::spawn(writer.run(MediaKind::Microphone, rx, move |_, _| {
                let entered = entered.clone();
                let ready = ready.clone();
                let sent = sent.clone();
                async move {
                    entered.try_send(()).unwrap();
                    ready.notified().await;
                    sent.fetch_add(1, Ordering::Relaxed);
                    Ok(())
                }
            }));
            waiting.recv().await.unwrap();
            {
                let mut members = room.members.write().await;
                let member = members.get_mut(&changed_slot).unwrap();
                if changed_slot == 0 {
                    member.packet_redundancy = true;
                    assert!(red::accept_boundary(&room, member, 1));
                } else {
                    assert!(red::boundary(&room, member));
                }
            }
            resume.notify_one();
            task.await.unwrap();
            assert!(active.load(Ordering::Acquire));
            assert_eq!(
                writes.load(Ordering::Relaxed),
                0,
                "pending packet crossed privacy boundary"
            );
        }
    }

    #[tokio::test]
    async fn independent_writers_reject_revoked_deafened_stopped_and_replaced_sources() {
        for kind in [
            MediaKind::Microphone,
            MediaKind::ScreenVideo,
            MediaKind::SystemAudio,
        ] {
            for reason in 0..5 {
                let (writer, source, _) = fixture().await;
                let (tx, rx) = channel(kind);
                let mut packet = delivery(kind, &source, 100);
                {
                    let mut members = writer.room.members.write().await;
                    match reason {
                        0 => members.get_mut(&0).unwrap().info.deafened = true,
                        1 => members.get_mut(&1).unwrap().info.can_speak = false,
                        2 => members.get_mut(&1).unwrap().source_id = uuid::Uuid::new_v4(),
                        3 => packet.epoch = packet.epoch.wrapping_add(1),
                        _ => packet.source_active.store(false, Ordering::Release),
                    }
                }
                tx.try_send(packet).unwrap();
                drop(tx);
                writer
                    .run(kind, rx, |_, _| async {
                        panic!("unauthorized packet sent")
                    })
                    .await;
            }
        }
    }
}
