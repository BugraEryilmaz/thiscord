use super::*;
use crate::audio::{connection::Connection, transmit::Encoder};
use std::time::Instant;
use thiscord_shared::voice::MediaKind;

async fn reported_peer(
    connection: Connection,
) -> (
    Arc<dyn PeerConnection>,
    Arc<Notify>,
    mpsc::Receiver<rtp::Packet>,
) {
    let (packets, received) = mpsc::channel(16);
    let gathered = Arc::new(Notify::new());
    let mut media = MediaEngine::default();
    media.register_default_codecs().unwrap();
    let registry = register_default_interceptors(Registry::new(), &mut media).unwrap();
    let registry = crate::audio::adaptive_fec::register(registry, connection);
    let peer = PeerConnectionBuilder::new()
        .with_media_engine(media)
        .with_interceptor_registry(registry)
        .with_handler(Arc::new(Handler {
            gathered: gathered.clone(),
            packets,
        }))
        .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
        .build()
        .await
        .unwrap();
    (Arc::new(peer), gathered, received)
}

/// Real encrypted receiver reports, not synthetic inbound listener statistics.
/// Sequence gaps emulate 20% uplink loss across a 16-bit sequence wrap.
#[tokio::test]
async fn adaptive_fec_uses_real_rtcp_and_rejects_closed_peer_feedback() {
    let mut peers = Vec::new();
    let result = tokio::time::timeout(Duration::from_secs(20), async {
        let connection = Connection::default();
        let (sender, sg, _) = reported_peer(connection.clone()).await;
        let (receiver, rg, mut received) = reported_peer(Connection::default()).await;
        peers.extend([sender.clone(), receiver.clone()]);
        let microphone = track(MediaKind::Microphone.publisher_ssrc());
        sender.add_track(microphone.clone() as Arc<dyn TrackLocal>).await.unwrap();
        connect(&sender, &sg, &receiver, &rg).await.unwrap();
        let mut encoder = Encoder::new(false).unwrap();
        let mut bytes = [0; 4000];
        let (n, _, _) = encoder.encode(&[0.0; 960], true, &mut bytes).unwrap();
        let payload = bytes::Bytes::copy_from_slice(&bytes[..n]);
        // Wait for transport before starting the deterministic loss pattern.
        let mut tick = tokio::time::interval(Duration::from_millis(20));
        loop {
            tokio::select! {
                _ = tick.tick() => { microphone.write_rtp(rtp::Packet {
                    header: rtp::header::Header { version: 2, sequence_number: 65_500,
                        timestamp: 0, ssrc: MediaKind::Microphone.publisher_ssrc(), payload_type: 111, ..Default::default() }, payload: payload.clone()
                }).await.unwrap(); },
                Some(_) = received.recv() => break,
            }
        }
        connection.uplink_sent(0, 65_500);
        for i in 1..=400u32 {
            tick.tick().await;
            if !i.is_multiple_of(5) {
                microphone.write_rtp(rtp::Packet {
                    header: rtp::header::Header { version: 2, sequence_number: (65_500 + i) as u16,
                        timestamp: i * 960, ssrc: MediaKind::Microphone.publisher_ssrc(), payload_type: 111, ..Default::default() }, payload: payload.clone()
                }).await.unwrap();
                connection.uplink_sent(0, (65_500 + i) as u16);
            }
            while received.try_recv().is_ok() {}
            if i.is_multiple_of(50) {
                encoder.adapt(true, 5, connection.uplink_sample(0), Instant::now()).unwrap();
            }
        }
        assert!(connection.uplink_sample(0).is_some(), "the real peer must supply usable microphone receiver reports");
        assert!(encoder.codec.get_packet_loss_perc().unwrap() > 5, "server-observed gaps must change the live encoder");
        let hint = encoder.codec.get_packet_loss_perc().unwrap();
        // Source replacement starts empty even while the previous peer is live.
        assert!(Connection::default().uplink_sample(0).is_none());
        connection.close();
        assert!(connection.uplink_sample(0).is_none());
        encoder.adapt(true, 5, connection.uplink_sample(0), Instant::now()).unwrap();
        assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), 5);
        eprintln!("real encrypted RTCP: 20% deterministic uplink sequence gaps; effective hint after 8s={hint}%; closed peer fallback=5%");
    }).await;
    for peer in peers {
        let _ = peer.close().await;
    }
    assert!(result.is_ok(), "receiver report integration timed out");
}
