//! In-loop fan-out for the shard transport: the webrtc mode's relay task (the
//! `relay` loop in `serve`) followed by its egress writers
//! (`egress::Writer::run`), for one packet, synchronously, under one room read
//! lock, writing into each receiver's core on the same thread.
//!
//! The authorization, filtering, RED, layer-route, sequence-rewrite and byte
//! budget steps are the same, in the same order, using the same helpers;
//! admission is literally `egress::Sender::admit`. What differs is only what the
//! queue between relay and writer used to imply:
//! - No queue: the queue's packet/byte capacity and age bounds cannot be
//!   exceeded, so `Sender::try_send` is not applied. The writer's deadline,
//!   which also covers packets held back by a screen route, still is.
//! - The writer's revalidation runs under the same lock acquisition as the
//!   admission. Both checks still run, so neither can be skipped by a change
//!   that only affects one of them.
//! - No transport queue to be full: a write is never "pending", so the
//!   webrtc mode's send-timeout path (metrics[4], SCREEN_BACKOFF) has no
//!   trigger. Socket buffer overflow drops datagrams in `Shard::flush`.
use super::*;
use rtc::shared::marshal::MarshalSize;

/// One receiver's writer state for one media kind (`Writer::run`'s locals).
#[derive(Default)]
pub(super) struct Egress {
    sources: [Option<uuid::Uuid>; ROOM_CAPACITY],
    red_floors: [red::Floor; ROOM_CAPACITY],
    offsets: [(u16, u32); ROOM_CAPACITY],
    last: [(u16, u32); ROOM_CAPACITY],
    retry_after: [Option<Instant>; ROOM_CAPACITY],
    screen_sequences: [layers::SequenceWindow; ROOM_CAPACITY],
    screen_recovered_at: [Option<Instant>; ROOM_CAPACITY],
    screen_budgets: [viewing::ByteBudget; ROOM_CAPACITY],
}

impl Shard {
    pub(super) async fn forward(
        &mut self,
        source_peer: PeerId,
        kind: MediaKind,
        mut packet: rtp::Packet,
        arrived: Instant,
    ) {
        let Some(connection) = self.get_mut(source_peer) else {
            return;
        };
        if !connection.relaying {
            return;
        }
        connection.performance_sample = connection.performance_sample.wrapping_add(1);
        let sampled = connection.performance_sample.is_multiple_of(64);
        let browser = connection.browser;
        let context = &connection.context;
        let (room, slot, source_id) = (context.room.clone(), context.slot, context.source_id);
        let relay_access = context.access.clone();
        let relay_active = context.active.clone();
        if !relay_active.load(Ordering::Acquire) {
            connection.relaying = false;
            return;
        }
        // The webrtc mode records the epoch at ingress and compares it with the
        // current one in the relay; here both are the same instant.
        let epoch = context.generation.load(Ordering::Acquire);
        let Some(_permit) = relay_access.packet(epoch, epoch) else {
            return;
        };
        if sampled {
            crate::performance::SFU_QUEUE.observe(arrived.elapsed());
        }
        // Packet-only parsing needs no room state; keep it outside the lock.
        // A browser-chosen SSRC can equal the native low-layer SSRC.
        let layer =
            usize::from(!browser && packet.header.ssrc == thiscord_shared::screen::LOW_LAYER_SSRC);
        let layer_packet = thiscord_shared::screen::LayerPacket::parse(&packet.header.csrc);
        let lock_timing = sampled.then(|| crate::performance::SFU_LOCK.start());
        let members = match room.members.try_read() {
            Ok(members) => members,
            Err(_) => {
                let members = room.members.read().await;
                self.now = Instant::now();
                members
            }
        };
        drop(lock_timing);
        let fanout_timing = sampled.then(|| crate::performance::SFU_FANOUT.start());
        let Some(source) = members.get(&slot) else {
            self.stop_relaying(source_peer);
            return;
        };
        if source.source_id != source_id || !relay_active.load(Ordering::Acquire) {
            self.stop_relaying(source_peer);
            return;
        }
        if !may_relay(source, kind, arrived) {
            return;
        }
        if kind == MediaKind::Microphone
            && (!red::valid_ingress(&packet, source.packet_redundancy)
                || !red::sender_generation(&packet, source))
        {
            return;
        }
        if kind == MediaKind::ScreenVideo
            && ((layer == 1 && !source.screen_layers)
                || (source.screen_layers
                    && layer_packet.is_none_or(|p| p.epoch != source.info.screen_epoch)))
        {
            return;
        }
        packet.header = rtp::header::Header {
            version: 2,
            payload_type: if kind == MediaKind::Microphone {
                packet.header.payload_type
            } else {
                kind.payload_type()
            },
            ssrc: media_ssrc(kind.track_index(slot)),
            marker: packet.header.marker,
            csrc: if kind == MediaKind::Microphone {
                vec![
                    source.info.microphone_source_epoch,
                    source.info.microphone_epoch,
                ]
            } else if kind == MediaKind::ScreenVideo && source.screen_layers {
                layer_packet.unwrap().csrc()
            } else {
                vec![source.info.video_epoch(kind)]
            },
            sequence_number: packet.header.sequence_number,
            timestamp: packet.header.timestamp,
            ..Default::default()
        };
        let layered = kind == MediaKind::ScreenVideo && source.screen_layers;
        let plan = if layered {
            viewing::layer_plan(&members, source)
        } else {
            Default::default()
        };
        let receivers = self
            .rooms
            .get(&(Arc::as_ptr(&room) as usize))
            .copied()
            .unwrap_or_default();
        let mut recovery = std::mem::take(&mut self.recovery);
        for (&other, m) in members.iter() {
            if other == slot {
                continue;
            }
            // Only a connection that belongs to this member and has finished
            // negotiating receives (the webrtc mode has no writer before that).
            let Some(receiver) = receivers.get(other).copied().flatten().filter(|&id| {
                self.get(id)
                    .is_some_and(|c| c.forwarding && c.context.source_id == m.source_id)
            }) else {
                continue;
            };
            let receiver_epoch = m.generation.load(Ordering::Acquire);
            let screen = kind == MediaKind::ScreenVideo;
            let single = (!screen).then(|| (packet.clone(), arrived));
            let packets = if screen {
                let mut routes = m.screen_routes.lock().unwrap();
                if !viewing::receives(m, source, kind, arrived) {
                    routes[slot].clear();
                    continue;
                }
                let view = m.views[kind.view_index(slot)].unwrap();
                let identity = (
                    source_id,
                    source.info.screen_epoch,
                    view.since,
                    epoch,
                    receiver_epoch,
                );
                let packets = if layered {
                    routes[slot].push_at(
                        identity,
                        plan.layer(view.subscription.bitrate),
                        layer,
                        packet.clone(),
                        arrived,
                    )
                } else {
                    routes[slot].push_legacy_at(identity, packet.clone(), arrived)
                };
                let queued_at = routes[slot].released_at.unwrap_or(arrived);
                packets.into_iter().map(|p| (p, queued_at)).collect()
            } else {
                Vec::new()
            };
            let tx = &m.outgoing[kind as usize];
            for (selected, queued_at) in packets.into_iter().chain(single) {
                let result = tx.admit(
                    source,
                    m,
                    Delivery {
                        epoch,
                        receiver_epoch,
                        source_access: relay_access.clone(),
                        slot: kind.track_index(slot),
                        source: source_id,
                        source_active: relay_active.clone(),
                        packet: selected,
                        screen_layer: layered.then_some(layer),
                        queued_at,
                    },
                );
                match result {
                    Ok(Some(admitted)) => {
                        let (delivery, _permits) = admitted.into_parts();
                        let now = self.now;
                        let Some(connection) = self.get_mut(receiver) else {
                            continue;
                        };
                        if deliver(
                            connection,
                            now,
                            &members,
                            other,
                            kind,
                            delivery,
                            &room,
                            &mut recovery,
                        ) {
                            self.output(receiver);
                        }
                    }
                    Ok(None) => {}
                    // Video congestion asks the publisher for a keyframe.
                    Err(()) => {
                        if kind.is_video() {
                            m.metrics[2].fetch_add(1, Ordering::Relaxed);
                            recovery.push(Recovery {
                                room: room.clone(),
                                viewer: other,
                                publisher: slot,
                                kind,
                                epoch: source.info.video_epoch(kind),
                                active: m.active.clone(),
                            });
                        }
                    }
                }
            }
        }
        drop(members);
        drop(fanout_timing);
        self.recovery = recovery;
    }

    fn stop_relaying(&mut self, id: PeerId) {
        if let Some(connection) = self.get_mut(id) {
            connection.relaying = false;
        }
    }
}

fn dropped(
    connection: &Connection,
    recovery: &mut Vec<Recovery>,
    room: &Arc<Room>,
    receiver_slot: usize,
    source_slot: usize,
    kind: MediaKind,
    epoch: u32,
) {
    if kind.is_video() {
        connection.context.metrics[2].fetch_add(1, Ordering::Relaxed);
        recovery.push(Recovery {
            room: room.clone(),
            viewer: receiver_slot,
            publisher: source_slot,
            kind,
            epoch,
            active: connection.context.active.clone(),
        });
    }
}

/// `egress::Writer::run` for one delivery. Returns whether the receiver's
/// core was written to (and needs its output drained).
#[allow(clippy::too_many_arguments)]
fn deliver(
    connection: &mut Connection,
    now: Instant,
    members: &HashMap<usize, Member>,
    receiver_slot: usize,
    kind: MediaKind,
    delivery: Delivery,
    room: &Arc<Room>,
    recovery: &mut Vec<Recovery>,
) -> bool {
    let Delivery {
        epoch,
        receiver_epoch,
        source_access,
        slot,
        source,
        source_active,
        mut packet,
        screen_layer,
        queued_at,
    } = delivery;
    if !connection.context.active.load(Ordering::Acquire) {
        return false;
    }
    let (packet_kind, source_slot) = MediaKind::from_track(slot).expect("delivery track");
    debug_assert_eq!(kind, packet_kind);
    if kind == MediaKind::ScreenVideo {
        packet.header.csrc.truncate(1);
    }
    let source_epoch = packet.header.csrc.first().copied();
    let packet_epoch = packet
        .header
        .csrc
        .get(if kind == MediaKind::Microphone { 1 } else { 0 })
        .copied();
    let screen_epoch = packet_epoch.unwrap_or(0);
    let state = &mut connection.egress[kind as usize];
    let receiver_microphone_epoch = if kind == MediaKind::Microphone {
        let Some(publisher) = members.get(&source_slot) else {
            return false;
        };
        let Some(receiver) = members.get(&receiver_slot) else {
            return false;
        };
        if publisher.source_id != source
            || packet_epoch != Some(publisher.info.microphone_epoch)
            || source_epoch != Some(publisher.info.microphone_source_epoch)
            || queued_at < receiver.red.receiver_started
            || !viewing::receives(receiver, publisher, kind, queued_at)
            || !red::receives(publisher, receiver)
            || !state.red_floors[source_slot].prepare(
                &mut packet,
                source,
                publisher.info.microphone_epoch,
                receiver.red.receiver_floor,
                receiver.packet_redundancy,
            )
        {
            return false;
        }
        // See `Writer::run`: a globally ordered effective privacy revision.
        if publisher.packet_redundancy {
            packet.header.csrc[1] = publisher
                .info
                .microphone_epoch
                .max(receiver.red.receiver_floor);
        } else {
            packet.header.csrc.clear();
        }
        Some(receiver.red.receiver_floor)
    } else {
        None
    };
    if state.sources[source_slot] != Some(source) {
        state.sources[source_slot] = Some(source);
        state.retry_after[source_slot] = None;
        state.offsets[source_slot] = (
            state.last[source_slot]
                .0
                .wrapping_add(1)
                .wrapping_sub(packet.header.sequence_number),
            state.last[source_slot]
                .1
                .wrapping_add(if kind.is_video() { 9000 } else { 960 })
                .wrapping_sub(packet.header.timestamp),
        );
    }
    let deadline = queued_at + egress::max_age(kind);
    if now >= deadline || state.retry_after[source_slot].is_some_and(|at| queued_at < at) {
        dropped(
            connection,
            recovery,
            room,
            receiver_slot,
            source_slot,
            kind,
            screen_epoch,
        );
        return false;
    }
    // Preserve SRTP sequence/timestamp continuity on slot reuse.
    packet.header.sequence_number = packet
        .header
        .sequence_number
        .wrapping_add(state.offsets[source_slot].0);
    packet.header.timestamp = packet
        .header
        .timestamp
        .wrapping_add(state.offsets[source_slot].1);
    if packet
        .header
        .sequence_number
        .wrapping_sub(state.last[source_slot].0) as i16
        > 0
        && kind != MediaKind::ScreenVideo
    {
        state.last[source_slot] = (packet.header.sequence_number, packet.header.timestamp);
    }
    let mut screen_sequence = state.screen_sequences[source_slot].clone();
    let loss_at = if kind == MediaKind::ScreenVideo {
        members.get(&receiver_slot).and_then(|receiver| {
            receiver.outgoing[kind as usize]
                .screen_budget
                .lock()
                .unwrap()[source_slot]
                .rejected_at
        })
    } else {
        None
    };
    let mut recovery_offset = 0;
    if kind == MediaKind::ScreenVideo
        && loss_at
            .is_some_and(|at| queued_at >= at && Some(at) != state.screen_recovered_at[source_slot])
        && let Some(next) = screen_sequence.after_loss()
    {
        recovery_offset = next.wrapping_sub(packet.header.sequence_number);
        packet.header.sequence_number = next;
    }
    if kind == MediaKind::ScreenVideo
        && screen_sequence
            .accept(packet.header.sequence_number)
            .is_none()
    {
        if screen_sequence.discontinuity(packet.header.sequence_number)
            && let Some(receiver) = members.get(&receiver_slot)
        {
            receiver.outgoing[kind as usize]
                .screen_budget
                .lock()
                .unwrap()[source_slot]
                .rejected_at = Some(Instant::now());
            receiver.screen_routes.lock().unwrap()[source_slot].clear();
        }
        dropped(
            connection,
            recovery,
            room,
            receiver_slot,
            source_slot,
            kind,
            screen_epoch,
        );
        return false;
    }
    let sent_position = (packet.header.sequence_number, packet.header.timestamp);
    let measured_bytes = packet.marshal_size().saturating_add(64);
    // `authorized_write`: current permits, then the room-state predicate.
    let permits = delivery_permits(
        &source_access,
        epoch,
        &connection.context.access,
        receiver_epoch,
        connection.context.generation.load(Ordering::Acquire),
    );
    let mut rate_rejected = false;
    let allowed = permits.is_some()
        && now < deadline
        && connection.context.active.load(Ordering::Acquire)
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
                && members.get(&receiver_slot).is_some_and(|receiver| {
                    (kind != MediaKind::Microphone
                        || (receiver_microphone_epoch == Some(receiver.red.receiver_floor)
                            && queued_at >= receiver.red.receiver_started))
                        && viewing::selected_layer(members, receiver, m, kind, screen_layer)
                        && viewing::receives(receiver, m, kind, queued_at)
                        && (kind != MediaKind::Microphone || red::receives(m, receiver))
                        && (kind != MediaKind::ScreenVideo || {
                            let budget = &receiver.outgoing[kind as usize].screen_budget;
                            let fresh = budget.lock().unwrap()[source_slot]
                                .rejected_at
                                .is_none_or(|at| queued_at >= at);
                            let bitrate = receiver.views[source_slot].unwrap().subscription.bitrate;
                            let admitted = fresh
                                && state.screen_budgets[source_slot].admit(
                                    bitrate,
                                    measured_bytes,
                                    now,
                                );
                            if !admitted && fresh {
                                rate_rejected = true;
                                budget.lock().unwrap()[source_slot].rejected_at = Some(now);
                                receiver.screen_routes.lock().unwrap()[source_slot].clear();
                            }
                            admitted
                        })
                })
        });
    if !allowed {
        if now >= deadline || rate_rejected {
            dropped(
                connection,
                recovery,
                room,
                receiver_slot,
                source_slot,
                kind,
                screen_epoch,
            );
        }
        return false;
    }
    let Some(&sender) = connection.senders.get(slot) else {
        return false;
    };
    // The webrtc driver logs and discards core write errors after the track
    // write has already succeeded; the writer never observes them.
    if let Some(mut sender) = connection.core.rtp_sender(sender)
        && let Err(error) = sender.write_rtp(now, packet)
    {
        tracing::trace!(%error, "voice relay write rejected by transport");
    }
    drop(permits);
    let state = &mut connection.egress[kind as usize];
    if kind.is_video() {
        if kind == MediaKind::ScreenVideo {
            state.screen_sequences[source_slot] = screen_sequence;
            state.screen_recovered_at[source_slot] = loss_at;
            state.offsets[source_slot].0 =
                state.offsets[source_slot].0.wrapping_add(recovery_offset);
            if sent_position.0.wrapping_sub(state.last[source_slot].0) as i16 > 0 {
                state.last[source_slot] = sent_position;
            }
        }
        connection.context.metrics[3].fetch_add(1, Ordering::Relaxed);
    }
    true
}
