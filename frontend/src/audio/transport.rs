//! Loopback-only feasibility probe. This is not an authenticated production SFU.
#[cfg(test)]
#[path = "adaptive_fec_transport_tests.rs"]
mod adaptive_fec_tests;
use rtc::{
    media_stream::MediaStreamTrack,
    rtp,
    rtp_transceiver::rtp_sender::{
        RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
    },
};
use std::{sync::Arc, time::Duration};
use tokio::sync::{Notify, mpsc};
use webrtc::{
    media_stream::{
        track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP},
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::*,
};

struct Handler {
    gathered: Arc<Notify>,
    packets: mpsc::Sender<rtp::Packet>,
}
#[async_trait::async_trait]
impl PeerConnectionEventHandler for Handler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            self.gathered.notify_one();
        }
    }
    async fn on_track(&self, track: Arc<dyn TrackRemote>) {
        let packets = self.packets.clone();
        tokio::spawn(async move {
            while let Some(event) = track.poll().await {
                if let TrackRemoteEvent::OnRtpPacket(packet) = event {
                    let _ = packets.try_send(packet);
                }
            }
        });
    }
}
fn codec() -> RTCRtpCodec {
    RTCRtpCodec {
        mime_type: "audio/opus".into(),
        clock_rate: 48_000,
        channels: 2,
        sdp_fmtp_line: "minptime=10;useinbandfec=1".into(),
        ..Default::default()
    }
}
pub fn track(ssrc: u32) -> Arc<TrackLocalStaticRTP> {
    Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
        format!("stream-{ssrc}"),
        format!("track-{ssrc}"),
        format!("stream-{ssrc}"),
        RtpCodecKind::Audio,
        vec![RTCRtpEncodingParameters {
            rtp_coding_parameters: RTCRtpCodingParameters {
                ssrc: Some(ssrc),
                ..Default::default()
            },
            codec: codec(),
            ..Default::default()
        }],
    )))
}
async fn peer() -> Result<
    (
        Arc<dyn PeerConnection>,
        Arc<Notify>,
        mpsc::Receiver<rtp::Packet>,
    ),
    String,
> {
    let (tx, rx) = mpsc::channel(16);
    let gathered = Arc::new(Notify::new());
    let mut engine = MediaEngine::default();
    engine
        .register_default_codecs()
        .map_err(|e| e.to_string())?;
    let p = PeerConnectionBuilder::new()
        .with_media_engine(engine)
        .with_handler(Arc::new(Handler {
            gathered: gathered.clone(),
            packets: tx,
        }))
        .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
        .build()
        .await
        .map_err(|e| e.to_string())?;
    Ok((Arc::new(p), gathered, rx))
}
async fn connect(
    a: &Arc<dyn PeerConnection>,
    ag: &Notify,
    b: &Arc<dyn PeerConnection>,
    bg: &Notify,
) -> Result<(), String> {
    let offer = a.create_offer(None).await.map_err(|e| e.to_string())?;
    a.set_local_description(offer)
        .await
        .map_err(|e| e.to_string())?;
    ag.notified().await;
    b.set_remote_description(a.local_description().await.ok_or("Missing offer")?)
        .await
        .map_err(|e| e.to_string())?;
    let answer = b.create_answer(None).await.map_err(|e| e.to_string())?;
    b.set_local_description(answer)
        .await
        .map_err(|e| e.to_string())?;
    bg.notified().await;
    a.set_remote_description(b.local_description().await.ok_or("Missing answer")?)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
/// Sends Opus through two DTLS-SRTP hops, forwarding RTP without server decoding.
pub async fn probe() -> Result<String, String> {
    let mut peers = Vec::<Arc<dyn PeerConnection>>::new();
    let result=tokio::time::timeout(Duration::from_secs(15),async{
        let(a,ag,_)=peer().await?;peers.push(a.clone());
        let(b,bg,mut received)=peer().await?;peers.push(b.clone());
        let(c,cg,_)=peer().await?;peers.push(c.clone());
        let(d,dg,mut playback)=peer().await?;peers.push(d.clone());
        let sender=track(101);let forwarded=track(202);
        a.add_track(sender.clone() as Arc<dyn TrackLocal>).await.map_err(|e|e.to_string())?;
        c.add_track(forwarded.clone() as Arc<dyn TrackLocal>).await.map_err(|e|e.to_string())?;
        connect(&a,&ag,&b,&bg).await?;connect(&c,&cg,&d,&dg).await?;
        let mut encoder=opus::Encoder::new(48_000,opus::Channels::Mono,opus::Application::Voip).map_err(|e|e.to_string())?;
        let pcm:Vec<f32>=(0..960).map(|i|(i as f32*440.0*std::f32::consts::TAU/48_000.0).sin()*0.1).collect();
        let mut encoded=[0_u8;4000];let n=encoder.encode_float(&pcm,&mut encoded).map_err(|e|e.to_string())?;let encoded=bytes::Bytes::copy_from_slice(&encoded[..n]);
        let mut ticker=tokio::time::interval(Duration::from_millis(20));let mut sequence=0u16;
        loop{tokio::select!{
            _=ticker.tick()=>{sequence=sequence.wrapping_add(1);let packet=rtp::Packet{header:rtp::header::Header{version:2,payload_type:111,sequence_number:sequence,timestamp:sequence as u32*960,ssrc:101,..Default::default()},payload:encoded.clone()};sender.write_rtp(packet).await.map_err(|e|e.to_string())?;},
            Some(mut packet)=received.recv()=>{if packet.payload!=encoded{return Err("SFU input payload changed".into());}packet.header.ssrc=202;forwarded.write_rtp(packet).await.map_err(|e|e.to_string())?;},
            Some(packet)=playback.recv()=>{
                if packet.payload!=encoded{return Err("Forwarded Opus payload changed".into());}
                let mut decoder=opus::Decoder::new(48_000,opus::Channels::Mono).map_err(|e|e.to_string())?;let mut samples=[0.0_f32;960];
                let n=decoder.decode_float(&packet.payload,&mut samples,false).map_err(|e|e.to_string())?;
                if n!=960||!samples.iter().any(|s|s.abs()>0.001){return Err("Opus decode produced no audio".into());}
                return Ok("Local WebRTC probe passed: Opus forwarded across two encrypted hops".into());
            }
        }}
    }).await.map_err(|_|"WebRTC probe timed out".to_owned()).and_then(|v|v);
    for peer in peers {
        let _ = tokio::time::timeout(Duration::from_secs(2), peer.close()).await;
    }
    result
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn encrypted_opus_forwarding() {
        super::probe().await.unwrap();
    }

    #[tokio::test]
    async fn encrypted_connections_survive_31_seconds_without_audio_packets() {
        let mut peers = Vec::new();
        let result = tokio::time::timeout(Duration::from_secs(45), async {
            let (a, ag, _) = peer().await.unwrap();
            peers.push(a.clone());
            let (b, bg, mut ingress) = peer().await.unwrap();
            peers.push(b.clone());
            let (c, cg, _) = peer().await.unwrap();
            peers.push(c.clone());
            let (d, dg, mut egress) = peer().await.unwrap();
            peers.push(d.clone());
            let sender = track(101);
            let relay = track(202);
            a.add_track(sender.clone() as Arc<dyn TrackLocal>)
                .await
                .unwrap();
            c.add_track(relay.clone() as Arc<dyn TrackLocal>)
                .await
                .unwrap();
            connect(&a, &ag, &b, &bg).await.unwrap();
            connect(&c, &cg, &d, &dg).await.unwrap();
            let mut encoder = super::super::transmit::Encoder::new(true).unwrap();
            let pcm = std::array::from_fn(|i| (i as f32 * 0.04).sin() * 0.01);
            let mut output = [0; 4000];
            let (n, _, _) = encoder.encode(&pcm, true, &mut output).unwrap();
            let payload = bytes::Bytes::copy_from_slice(&output[..n]);
            // Initial packets establish both DTLS hops. No RTP at all during
            // the gap, even though normal DTX would also send periodic updates.
            for sequence in 1..=2 {
                let timestamp = 1000 + (sequence as u32 - 1) * 31 * 48_000;
                let packet = rtp::Packet {
                    header: rtp::header::Header {
                        version: 2,
                        payload_type: 111,
                        ssrc: 101,
                        sequence_number: sequence,
                        timestamp,
                        marker: true,
                        ..Default::default()
                    },
                    payload: payload.clone(),
                };
                // Peer gathering does not imply DTLS is ready yet; retries here
                // only establish the initial packet, never bridge the silent gap.
                let mut retry = tokio::time::interval(Duration::from_millis(100));
                let mut incoming = loop {
                    tokio::select! {
                        packet = ingress.recv() => break packet.unwrap(),
                        _ = retry.tick() => { sender.write_rtp(packet.clone()).await.unwrap(); }
                    }
                };
                incoming.header.ssrc = 202;
                let received = loop {
                    tokio::select! {
                        packet = egress.recv() => break packet.unwrap(),
                        _ = retry.tick() => { relay.write_rtp(incoming.clone()).await.unwrap(); }
                    }
                };
                assert_eq!(received.header.sequence_number, sequence);
                assert_eq!(received.header.timestamp, timestamp);
                assert_eq!(received.payload, payload);
                if sequence == 1 {
                    tokio::time::sleep(Duration::from_secs(31)).await;
                }
            }
        })
        .await;
        for peer in peers {
            let _ = peer.close().await;
        }
        assert!(result.is_ok(), "silence must not expire the transport");
    }
}
