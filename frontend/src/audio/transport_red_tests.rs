//! Real localhost DTLS-SRTP hops with deterministic packet loss between hops.
//! Reconstruct encoded bytes by timestamp only: these tests do not claim that
//! late redundancy is playable or synthesize sequence numbers for old blocks.
use super::*;
use bytes::Bytes;
use std::collections::HashMap;
use thiscord_shared::rtp_red::{self as red, Block};

fn require(value: bool, message: &str) -> Result<(), String> {
    value.then_some(()).ok_or_else(|| message.to_owned())
}

fn packet(sequence: u16, timestamp: u32, pt: u8, payload: Bytes) -> rtp::Packet {
    rtp::Packet {
        header: rtp::header::Header {
            version: 2,
            payload_type: pt,
            sequence_number: sequence,
            timestamp,
            ssrc: 101,
            ..Default::default()
        },
        payload,
    }
}

async fn receive(rx: &mut mpsc::Receiver<rtp::Packet>) -> Result<rtp::Packet, String> {
    tokio::time::timeout(Duration::from_secs(2), rx.recv())
        .await
        .map_err(|_| "Encrypted RTP receive timed out".to_owned())?
        .ok_or_else(|| "Encrypted RTP receiver closed".to_owned())
}

async fn warm(
    track: &TrackLocalStaticRTP,
    rx: &mut mpsc::Receiver<rtp::Packet>,
    packet: rtp::Packet,
) -> Result<(), String> {
    // Gathering completes before DTLS. Retry only the disposable warm-up
    // packet; the measured loss/reordering schedule never retries packets.
    let mut ticker = tokio::time::interval(Duration::from_millis(50));
    loop {
        tokio::select! {
            p = rx.recv() => {
                return require(p.is_some(), "Warm-up receiver closed");
            }
            _ = ticker.tick() => {
                track.write_rtp(packet.clone()).await.map_err(|e| e.to_string())?;
            }
        }
    }
}

async fn exercise(enabled: bool, old_server: bool, old_listener: bool) -> Result<(), String> {
    let mut peers = Vec::<Arc<dyn PeerConnection>>::new();
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let (a, ag, _) = peer_with_red(true).await?;
        peers.push(a.clone());
        let (b, bg, mut ingress) = peer_with_red(!old_server).await?;
        peers.push(b.clone());
        let (c, cg, _) = peer_with_red(true).await?;
        peers.push(c.clone());
        let (d, dg, mut egress) = peer_with_red(!old_listener).await?;
        peers.push(d.clone());
        let sender = track(101);
        let relay = track(202);
        a.add_track(sender.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|e| e.to_string())?;
        c.add_track(relay.clone() as Arc<dyn TrackLocal>)
            .await
            .map_err(|e| e.to_string())?;
        connect(&a, &ag, &b, &bg).await?;
        connect(&c, &cg, &d, &dg).await?;
        let offer = a.local_description().await.ok_or("Missing test offer")?;
        require(
            red::negotiated(&offer.sdp),
            "RED offer lacks PT112/48000/2 and fmtp111/111/111",
        )?;
        let upstream = red::negotiated(&a.remote_description().await.ok_or("Missing answer")?.sdp);
        let downstream =
            red::negotiated(&c.remote_description().await.ok_or("Missing answer")?.sdp);
        require(
            upstream != old_server,
            "Old server negotiation did not remove RED",
        )?;
        require(
            downstream != old_listener,
            "Old listener negotiation did not remove RED",
        )?;
        let use_red = enabled && upstream;

        let mut encoder = opus::Encoder::new(48_000, opus::Channels::Mono, opus::Application::Voip)
            .map_err(|e| e.to_string())?;
        encoder
            .set_bitrate(opus::Bitrate::Bits(32_000))
            .map_err(|e| e.to_string())?;
        encoder.set_vbr(false).map_err(|e| e.to_string())?;
        let mut frames = Vec::new();
        for index in 0_usize..12 {
            let pcm: Vec<_> = (0..960)
                .map(|sample| ((index * 960 + sample) as f32 * 0.057).sin() * 0.1)
                .collect();
            let mut output = [0; 4000];
            let n = encoder
                .encode_float(&pcm, &mut output)
                .map_err(|e| e.to_string())?;
            require(n == 80, "32 kbit/s CBR did not produce 80-byte Opus")?;
            frames.push(Bytes::copy_from_slice(&output[..n]));
        }
        let origin = u32::MAX - 5 * red::FRAME + 1;
        let remap = 12_345_u32;
        warm(
            &sender,
            &mut ingress,
            packet(
                65_520,
                origin.wrapping_sub(10 * red::FRAME),
                111,
                frames[0].clone(),
            ),
        )
        .await?;
        let mut warm_packet = packet(100, 0, 111, frames[0].clone());
        warm_packet.header.ssrc = 202;
        warm(&relay, &mut egress, warm_packet).await?;

        // An isolated loss (3), a two-packet burst (6,7), reordering (5,4),
        // and both sequence/timestamp wrap. Loss is injected after the first
        // encrypted hop; the remaining bytes cross a second encrypted hop.
        let mut captured = Vec::new();
        let mut offered_bytes = 0;
        for index in 0_usize..12 {
            let history: Vec<_> = (index.saturating_sub(2)..index)
                .map(|old| Block {
                    offset: ((index - old) as u32 * red::FRAME) as u16,
                    payload: &frames[old],
                })
                .collect();
            let payload = if use_red {
                Bytes::from(
                    red::encode(&frames[index], &history).ok_or("Cannot encode RED fixture")?,
                )
            } else {
                frames[index].clone()
            };
            offered_bytes += payload.len();
            let expected = packet(
                65_530_u16.wrapping_add(index as u16),
                origin.wrapping_add(index as u32 * red::FRAME),
                if use_red { 112 } else { 111 },
                payload,
            );
            sender
                .write_rtp(expected.clone())
                .await
                .map_err(|e| e.to_string())?;
            let got = receive(&mut ingress).await?;
            require(
                got.header.payload_type == expected.header.payload_type,
                "First hop changed negotiated payload type",
            )?;
            require(
                got.header.sequence_number == expected.header.sequence_number
                    && got.header.timestamp == expected.header.timestamp
                    && got.payload == expected.payload,
                "First hop changed RTP identity or encoded bytes",
            )?;
            captured.push(got);
        }
        require(
            offered_bytes == if use_red { 2736 } else { 960 },
            "Unexpected total sender payload budget",
        )?;
        let mut recovered = HashMap::new();
        let mut first_recovery_at = HashMap::new();
        let mut received_bytes = 0;
        let mut hop1_forwarded_bytes = 0;
        for index in [0, 1, 2, 5, 4, 8, 9, 10, 11] {
            let mut input = captured[index].clone();
            hop1_forwarded_bytes += input.payload.len();
            if input.header.payload_type == 112 && !downstream {
                input.payload = Bytes::copy_from_slice(
                    red::parse(&input.payload)
                        .ok_or("Invalid upstream RED")?
                        .primary,
                );
                input.header.payload_type = 111;
            }
            input.header.ssrc = 202;
            input.header.sequence_number = input.header.sequence_number.wrapping_add(1_000);
            input.header.timestamp = input.header.timestamp.wrapping_add(remap);
            relay
                .write_rtp(input.clone())
                .await
                .map_err(|e| e.to_string())?;
            let got = receive(&mut egress).await?;
            require(
                got.header.payload_type == input.header.payload_type && got.header.ssrc == 202,
                "Second hop changed negotiated RED type or SSRC",
            )?;
            require(
                got.header.sequence_number == input.header.sequence_number
                    && got.header.timestamp == input.header.timestamp
                    && got.payload == input.payload,
                "SFU remapping changed RED offsets or RTP continuity",
            )?;
            received_bytes += got.payload.len();
            let timestamp = got.header.timestamp.wrapping_sub(remap);
            if got.header.payload_type == 112 {
                let parsed = red::parse(&got.payload).ok_or("Invalid forwarded RED")?;
                recovered.insert(timestamp, parsed.primary.to_vec());
                first_recovery_at.entry(timestamp).or_insert(index);
                for block in parsed.history {
                    let old_timestamp = timestamp.wrapping_sub(u32::from(block.offset));
                    recovered.insert(old_timestamp, block.payload.to_vec());
                    first_recovery_at.entry(old_timestamp).or_insert(index);
                }
            } else {
                recovered.insert(timestamp, got.payload.to_vec());
                first_recovery_at.entry(timestamp).or_insert(index);
            }
        }
        require(
            hop1_forwarded_bytes == if use_red { 1989 } else { 720 },
            "Unexpected first-hop payload count",
        )?;
        let protection = use_red && downstream;
        require(
            received_bytes == if protection { 1989 } else { 720 },
            "Unexpected second-hop payload count",
        )?;
        require(
            recovered.len() == if protection { 12 } else { 9 },
            "Unexpected deterministic recovery count",
        )?;
        for (index, frame) in frames.iter().enumerate() {
            if protection || ![3, 6, 7].contains(&index) {
                require(
                    recovered
                        .get(&origin.wrapping_add(index as u32 * red::FRAME))
                        .is_some_and(|p| p == frame.as_ref()),
                    "Timestamp recovery changed Opus bytes",
                )?;
            }
        }
        if protection {
            // Media-time wait for the first surviving primary carrying each
            // lost frame. This is not wall-clock/end-to-end playout latency.
            for (lost, delay_ms) in [(3, 40), (6, 40), (7, 20)] {
                let timestamp = origin.wrapping_add(lost as u32 * red::FRAME);
                let first = *first_recovery_at
                    .get(&timestamp)
                    .ok_or("Lost frame unrecovered")?;
                require(
                    (first - lost) * 20 == delay_ms,
                    "Unexpected RED media-time recovery delay",
                )?;
            }
        }

        // A DTX-sized timestamp jump must remain a jump, with no redundant
        // samples invented for omitted audio. The primary marker is preserved.
        let payload = if use_red {
            Bytes::from(red::encode(&frames[0], &[]).ok_or("Cannot encode DTX resume")?)
        } else {
            frames[0].clone()
        };
        let mut resume = packet(
            6,
            origin.wrapping_add(22 * red::FRAME),
            if use_red { 112 } else { 111 },
            payload,
        );
        resume.header.marker = true;
        sender.write_rtp(resume).await.map_err(|e| e.to_string())?;
        let mut resumed = receive(&mut ingress).await?;
        if use_red {
            require(
                red::parse(&resumed.payload)
                    .ok_or("Invalid resume")?
                    .history
                    .is_empty(),
                "DTX omission advertised recovery",
            )?;
            if !downstream {
                resumed.payload = frames[0].clone();
                resumed.header.payload_type = 111;
            }
        }
        resumed.header.ssrc = 202;
        resumed.header.sequence_number = resumed.header.sequence_number.wrapping_add(1_000);
        resumed.header.timestamp = resumed.header.timestamp.wrapping_add(remap);
        relay
            .write_rtp(resumed.clone())
            .await
            .map_err(|e| e.to_string())?;
        let got = receive(&mut egress).await?;
        require(
            got.header.marker
                && got.header.sequence_number == 1_006
                && got.header.timestamp == resumed.header.timestamp,
            "DTX resume marker or sequence was synthesized",
        )?;

        if use_red {
            // Valid SRTP authentication does not make a malformed RED body safe.
            // The relay rejects these before forwarding; no decoder is used.
            let malformed = [vec![239], vec![112, 1], vec![111; red::MAX_PAYLOAD + 1]];
            for (index, payload) in malformed.into_iter().enumerate() {
                sender
                    .write_rtp(packet(
                        7 + index as u16,
                        origin.wrapping_add((23 + index as u32) * red::FRAME),
                        112,
                        payload.into(),
                    ))
                    .await
                    .map_err(|e| e.to_string())?;
                let got = receive(&mut ingress).await?;
                require(
                    red::parse(&got.payload).is_none(),
                    "Malformed RED passed bounded parser",
                )?;
            }
            require(
                tokio::time::timeout(Duration::from_millis(50), egress.recv())
                    .await
                    .is_err(),
                "Rejected RED reached the listener",
            )?;
        }
        Ok::<_, String>(())
    })
    .await
    .map_err(|_| "Two-hop RED test exceeded 15 seconds".to_owned())
    .and_then(|r| r);
    for peer in peers {
        let _ = tokio::time::timeout(Duration::from_secs(1), peer.close()).await;
    }
    result
}

#[tokio::test]
async fn encrypted_red_two_hop_loss_reorder_wrap_and_malformed() {
    exercise(true, false, false).await.unwrap();
}

#[tokio::test]
async fn encrypted_red_disabled_preserves_raw_opus() {
    exercise(false, false, false).await.unwrap();
}

#[tokio::test]
async fn encrypted_red_old_server_falls_back_before_sending() {
    exercise(true, true, false).await.unwrap();
}

#[tokio::test]
async fn encrypted_red_old_listener_receives_only_primary() {
    exercise(true, false, true).await.unwrap();
}
