//! Independent, bounded media writers. Screen traffic never waits for driver capacity.
use super::*;
use rtc::shared::marshal::MarshalSize;
use std::{future::Future, task::Poll};
use tokio::sync::OwnedSemaphorePermit;

const AUDIO_AGE: Duration = Duration::from_millis(100);
const VIDEO_AGE: Duration = Duration::from_millis(150);
const SCREEN_BACKOFF: Duration = Duration::from_millis(20);

fn max_age(kind: MediaKind) -> Duration {
    if kind == MediaKind::ScreenVideo {
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
    let (packets, bytes) = if kind == MediaKind::ScreenVideo {
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
            || delivery.epoch != receiver.generation.load(Ordering::Acquire)
            || (self.kind != MediaKind::Microphone
                && delivery.packet.header.csrc.first().copied() != Some(source.info.screen_epoch))
            || !viewing::receives(receiver, source, self.kind, delivery.queued_at)
        {
            return Ok(());
        }
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
        let mut offsets = [(0_u16, 0_u32); ROOM_CAPACITY];
        let mut last = [(0_u16, 0_u32); ROOM_CAPACITY];
        let mut retry_after = [None; ROOM_CAPACITY];
        while let Some(Queued { delivery, _bytes }) = rx.recv().await {
            let Delivery {
                epoch,
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
            let packet_epoch = packet.header.csrc.first().copied();
            let screen_epoch = packet_epoch.unwrap_or(0);
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
                        .wrapping_add(if kind == MediaKind::ScreenVideo {
                            9000
                        } else {
                            960
                        })
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
                    || access::global().packet(epoch, self.generation.load(Ordering::Acquire)),
                    |members| {
                        Instant::now() < deadline
                            && self.active.load(Ordering::Acquire)
                            && source_active.load(Ordering::Acquire)
                            && members.get(&source_slot).is_some_and(|m| {
                                m.source_id == source
                                    && (kind == MediaKind::Microphone
                                        || packet_epoch == Some(m.info.screen_epoch))
                                    && members.get(&self.receiver_slot).is_some_and(|receiver| {
                                        viewing::receives(receiver, m, kind, queued_at)
                                    })
                            })
                    },
                    write_media(kind, send(slot, packet)),
                ),
            )
            .await;
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
                if kind == MediaKind::ScreenVideo && delivered.is_err() {
                    self.metrics[4].fetch_add(1, Ordering::Relaxed);
                }
                self.dropped(kind, source_slot, screen_epoch).await;
                continue;
            }
            if kind == MediaKind::ScreenVideo {
                self.metrics[3].fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    async fn dropped(&self, kind: MediaKind, source_slot: usize, epoch: u32) {
        if kind == MediaKind::ScreenVideo {
            self.metrics[2].fetch_add(1, Ordering::Relaxed);
            request_keyframe(&self.room, self.receiver_slot, source_slot, epoch).await;
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
                    slot: 1,
                    owner: publisher.account_id,
                    epoch: publisher.screen_epoch,
                    bitrate: 2_500_000,
                }]
            )
            .await
        );
    }

    async fn change_viewer(room: &Room, change: ViewerChange) {
        match change {
            ViewerChange::Unsubscribe => {
                assert!(viewing::update(room, 0, vec![]).await);
            }
            ViewerChange::Expire => {
                room.members.write().await.get_mut(&0).unwrap().views[1]
                    .as_mut()
                    .unwrap()
                    .renewed -= Duration::from_secs(thiscord_shared::screen::VIEW_LEASE_SECS);
            }
            ViewerChange::Restart => {
                update_screen_state(room, 1, false, false).await.unwrap();
                update_screen_state(room, 1, true, true).await.unwrap();
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
        for kind in MediaKind::ALL {
            for change in [
                ViewerChange::Unsubscribe,
                ViewerChange::Expire,
                ViewerChange::Restart,
            ] {
                let (writer, source, _) = fixture().await;
                let (tx, mut rx) = channel(kind);
                let budget = tx.bytes.available_permits();
                forward(&writer.room, &tx, delivery(kind, source, 100)).await;
                drop(rx.try_recv().expect("active viewer must be admitted"));
                assert_eq!(tx.bytes.available_permits(), budget);
                change_viewer(&writer.room, change).await;
                // Timestamp after the mutation: restart must check the wire epoch,
                // not just whether there is now a viewer for this slot.
                forward(&writer.room, &tx, delivery(kind, source, 100)).await;
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
                let mut fresh = delivery(kind, source, 100);
                fresh.packet.header.csrc =
                    vec![writer.room.members.read().await[&1].info.screen_epoch];
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
        for kind in MediaKind::ALL {
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
                forward(&room, &tx, delivery(kind, source, 100)).await;
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
                        lock.get_mut(&0).unwrap().views = [None; ROOM_CAPACITY]
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
                    tx.try_send(delivery(kind, source, 100)).unwrap();
                    assert!(futures_util::poll!(run.as_mut()).is_pending());
                    assert!(
                        received.try_recv().is_err(),
                        "old share epoch reached transport"
                    );
                    assert_eq!(tx.bytes.available_permits(), budget);
                }
                subscribe(&room).await;
                let mut fresh = delivery(kind, source, 100);
                let epoch = room.members.read().await[&1].info.screen_epoch;
                fresh.packet.header.csrc = vec![epoch];
                forward(&room, &tx, fresh).await;
                drop(tx);
                run.await;
                assert_eq!(received.try_recv().unwrap().header.csrc, vec![epoch]);
                assert!(received.try_recv().is_err());
                assert!(active.load(Ordering::Acquire));
            }
        }
    }

    fn delivery(kind: MediaKind, source: uuid::Uuid, size: usize) -> Delivery {
        Delivery {
            epoch: access::global().snapshot().unwrap(),
            slot: kind.track_index(1),
            source,
            source_active: Arc::new(AtomicBool::new(true)),
            packet: rtp::Packet {
                header: rtp::header::Header {
                    version: 2,
                    csrc: vec![7],
                    sequence_number: 100,
                    timestamp: 1234,
                    ..Default::default()
                },
                payload: vec![0; size].into(),
            },
            queued_at: Instant::now(),
        }
    }

    async fn fixture() -> (Writer, uuid::Uuid, mpsc::Receiver<u32>) {
        let room = Arc::new(Room::default());
        let source = uuid::Uuid::new_v4();
        let epoch = access::global().snapshot().unwrap();
        let active = Arc::new(AtomicBool::new(true));
        let generation = Arc::new(AtomicU64::new(epoch));
        let metrics = Arc::new(std::array::from_fn(|_| AtomicU64::new(0)));
        let (keyframes, rx) = mpsc::channel(1);
        for slot in 0..2 {
            room.members.write().await.insert(
                slot,
                Member {
                    network: Arc::new(RwLock::new(Default::default())),
                    info: Participant {
                        account_id: uuid::Uuid::new_v4().to_string().parse().unwrap(),
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
                    },
                    source_id: source,
                    screen_started: Instant::now(),
                    outgoing: MediaKind::ALL.map(|kind| channel(kind).0),
                    keyframes: keyframes.clone(),
                    last_keyframe: None,
                    feedback_enabled: true,
                    subscriptions_enabled: true,
                    views: [None; ROOM_CAPACITY],
                    generation: generation.clone(),
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
                    slot: 1,
                    owner,
                    epoch: 7,
                    bitrate: 2_500_000,
                }]
            )
            .await
        );
        (
            Writer {
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
            room: writer.room.clone(),
            receiver_slot: writer.receiver_slot,
            active: writer.active.clone(),
            generation: writer.generation.clone(),
            metrics: writer.metrics.clone(),
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
            .try_send(delivery(MediaKind::ScreenVideo, source, 1000))
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
            .try_send(delivery(MediaKind::Microphone, source, 100))
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
            .try_send(delivery(MediaKind::ScreenVideo, source, 1000))
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
        for kind in MediaKind::ALL {
            let (writer, source, mut feedback) = fixture().await;
            let active = writer.active.clone();
            let (tx, rx) = channel(kind);
            tx.try_send(delivery(kind, source, 100)).unwrap();
            drop(tx);
            writer.run(kind, rx, |_, _| async { Err(()) }).await;
            assert_eq!(
                active.load(Ordering::Acquire),
                kind != MediaKind::Microphone
            );
            if kind == MediaKind::ScreenVideo {
                assert_eq!(feedback.try_recv().unwrap(), 7);
            }
        }
    }

    #[tokio::test]
    async fn byte_packet_and_age_budgets_release_on_drop_close_and_cancel() {
        for kind in MediaKind::ALL {
            let source = uuid::Uuid::new_v4();
            let (tx, mut rx) = channel(kind);
            let budget = tx.bytes.available_permits();
            let mut stale = delivery(kind, source, 1);
            stale.queued_at -= max_age(kind);
            assert!(tx.try_send(stale).is_err());
            assert!(tx.try_send(delivery(kind, source, budget)).is_err());
            let mut accepted = 0;
            while tx.try_send(delivery(kind, source, 1500)).is_ok() {
                accepted += 1;
            }
            assert!(accepted > 0);
            assert!(
                accepted < tx.tx.max_capacity(),
                "byte budget must bind before packet count"
            );
            let in_flight = rx.recv().await.unwrap();
            assert!(
                tx.try_send(delivery(kind, source, 1500)).is_err(),
                "in-flight bytes unaccounted"
            );
            let task = tokio::spawn(async move {
                let _in_flight = in_flight;
                std::future::pending::<()>().await;
            });
            task.abort();
            let _ = task.await;
            tx.try_send(delivery(kind, source, 1500)).unwrap();
            drop(rx);
            assert_eq!(tx.bytes.available_permits(), budget);
            assert!(tx.try_send(delivery(kind, source, 1)).is_err());
            assert_eq!(tx.bytes.available_permits(), budget);

            let (tx, rx) = channel(kind);
            for _ in 0..tx.tx.max_capacity() {
                tx.try_send(delivery(kind, source, 1)).unwrap();
            }
            assert!(tx.try_send(delivery(kind, source, 1)).is_err());
            drop(rx);
            assert_eq!(tx.bytes.available_permits(), budget);
        }
    }

    #[tokio::test]
    async fn queued_video_expires_without_touching_transport_and_requests_recovery() {
        let (writer, source, mut feedback) = fixture().await;
        let active = writer.active.clone();
        let (tx, mut rx) = channel(MediaKind::ScreenVideo);
        tx.try_send(delivery(MediaKind::ScreenVideo, source, 1000))
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
        assert_eq!(feedback.try_recv().unwrap(), 7);
        assert!(active.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn video_deadline_includes_waiting_for_room_access() {
        let (writer, source, mut feedback) = fixture().await;
        let room = writer.room.clone();
        let members = room.members.write().await;
        let (tx, rx) = channel(MediaKind::ScreenVideo);
        tx.try_send(delivery(MediaKind::ScreenVideo, source, 1000))
            .unwrap();
        drop(tx);
        let task = tokio::spawn(writer.run(MediaKind::ScreenVideo, rx, |_, _| async {
            panic!("late packet sent")
        }));
        tokio::time::sleep(VIDEO_AGE + Duration::from_millis(10)).await;
        drop(members);
        task.await.unwrap();
        assert_eq!(feedback.try_recv().unwrap(), 7);
    }

    #[tokio::test]
    async fn independent_writers_reject_revoked_deafened_stopped_and_replaced_sources() {
        for kind in MediaKind::ALL {
            for reason in 0..5 {
                let (writer, source, _) = fixture().await;
                let (tx, rx) = channel(kind);
                let mut packet = delivery(kind, source, 100);
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
