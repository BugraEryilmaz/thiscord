use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use diesel::{connection::SimpleConnection, prelude::*, sql_types::Text};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiscord_backend::{api, db, permissions};
use thiscord_shared::{AccountId, account::ACCOUNT_PATH, permissions::*};
use tower::ServiceExt;
use uuid::Uuid;
#[path = "voice/bots.rs"]
mod bots;
#[path = "voice/friends.rs"]
mod friends;
#[path = "voice/layers.rs"]
mod layers;
#[path = "voice/moderation.rs"]
mod moderation;
#[path = "voice/recovery.rs"]
mod recovery;
#[path = "voice/red.rs"]
mod red;
#[path = "voice/roster.rs"]
mod roster;
#[path = "voice/scripting_bots.rs"]
mod scripting_bots;

struct Database {
    connection: PgConnection,
    schema: String,
    pool: db::DbPool,
}
impl Drop for Database {
    fn drop(&mut self) {
        let statement = format!("DROP SCHEMA {} CASCADE", self.schema);
        for attempt in 0..3 {
            match self.connection.batch_execute(&statement) {
                // Background session checks can outlive a socket and acquire
                // table locks in a different order from DROP SCHEMA. PostgreSQL
                // rolls back the failed DROP, allowing that query to finish.
                Err(diesel::result::Error::DatabaseError(_, ref details))
                    if details.message() == "deadlock detected" && attempt < 2 => {}
                result => {
                    result.unwrap();
                    return;
                }
            }
        }
    }
}
fn database() -> Database {
    if std::env::var_os("TEST_DATABASE_URL").is_none() {
        dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    }
    let value = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let mut url = url::Url::parse(&value).unwrap();
    assert!(url.path().ends_with("_test"));
    let mut connection = PgConnection::establish(&value).unwrap();
    let schema = format!("voice_test_{}", Uuid::new_v4().simple());
    connection
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = db::connect_and_migrate(url.as_str()).unwrap();
    Database {
        connection,
        schema,
        pool,
    }
}
fn user(db: &Database, name: &str) -> (AccountId, String) {
    let id = AccountId::from_uuid(Uuid::new_v4());
    let token = URL_SAFE_NO_PAD.encode(Sha256::digest(Uuid::new_v4().as_bytes()));
    let sid = Uuid::new_v4();
    let mut c = db.pool.get().unwrap();
    diesel::sql_query("INSERT INTO accounts(id,username,email,display_name,email_verified) VALUES($1::uuid,$2,$2||'@example.test',$2,TRUE)").bind::<Text,_>(id.to_string()).bind::<Text,_>(name).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO identities(account_id,provider,subject,password_hash) VALUES($1::uuid,'password',$1,'test-unused-hash')").bind::<Text,_>(id.to_string()).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO sessions(id,account_id,device,reauthenticated_at) VALUES($1::uuid,$2::uuid,'test',now())").bind::<Text,_>(sid.to_string()).bind::<Text,_>(id.to_string()).execute(&mut c).unwrap();
    diesel::sql_query(
        "INSERT INTO session_tokens(token_hash,session_id,active) VALUES($1,$2::uuid,TRUE)",
    )
    .bind::<Text, _>(URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes())))
    .bind::<Text, _>(sid.to_string())
    .execute(&mut c)
    .unwrap();
    (id, token)
}
async fn call(app: &Router, path: &str, token: &str, command: Value) -> (StatusCode, Value) {
    // Isolated databases still share the production process-wide HTTP worker
    // limit. Bound parallel fixture commands so adding transport regressions
    // does not accidentally turn another fixture's setup into an overload test.
    static COMMANDS: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    let _command = COMMANDS
        .get_or_init(|| tokio::sync::Semaphore::new(4))
        .acquire()
        .await
        .unwrap();
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(command.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap()).unwrap();
    if !status.is_success() {
        assert_eq!(body["request_id"], request_id);
    }
    (status, body)
}
async fn command(app: &Router, token: &str, command: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, PERMISSIONS_PATH, token, command).await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn change(
    app: &Router,
    token: &str,
    state: &mut Value,
    change: Value,
    expected: StatusCode,
) -> Value {
    let body=command(app,token,json!({"action":"change","guild_id":state["guild"]["id"],"revision":state["guild"]["revision"],"change":change}),expected).await;
    if body["result"] == "state" {
        *state = body["state"].clone();
    } else if body["result"] == "moderation" {
        state["guild"] = body["state"]["guild"].clone();
    }
    body
}

use futures_util::{SinkExt, StreamExt};
use rtc::{media_stream::MediaStreamTrack, rtp, rtp_transceiver::rtp_sender::*};
use std::{sync::Arc, time::Duration};
use thiscord_shared::voice::{self, ClientEvent, ClientFrame, ServerEvent, ServerFrame};
use tokio::sync::{Notify, mpsc};
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};
use webrtc::{
    media_stream::{
        track_local::{TrackLocal, static_rtp::TrackLocalStaticRTP},
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::*,
};
type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
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
async fn event(socket: &mut Socket) -> ServerEvent {
    let frame = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let frame = socket.next().await.unwrap().unwrap();
            if !matches!(frame, Message::Ping(_) | Message::Pong(_)) {
                break frame;
            }
        }
    })
    .await
    .unwrap();
    serde_json::from_str::<ServerFrame>(frame.to_text().unwrap())
        .unwrap()
        .event
}
async fn send(socket: &mut Socket, event: ClientEvent) {
    socket
        .send(Message::Text(
            serde_json::to_string(&ClientFrame { version: 1, event })
                .unwrap()
                .into(),
        ))
        .await
        .unwrap();
}
async fn still_connected(socket: &mut Socket) {
    send(socket, ClientEvent::Ping {}).await;
    loop {
        match event(socket).await {
            ServerEvent::Pong {} => break,
            ServerEvent::Participants { .. }
            | ServerEvent::MediaDiagnostics { .. }
            | ServerEvent::ScreenTarget { .. } => {}
            ServerEvent::Revoked {} => panic!("Authorized voice connection was revoked"),
            _ => panic!("Unexpected event on an authorized voice connection"),
        }
    }
}
async fn join(addr: std::net::SocketAddr, token: &str, guild: Value, channel: Value) -> Socket {
    join_attempt(addr, token, guild, channel, None).await
}
async fn join_attempt(
    addr: std::net::SocketAddr,
    token: &str,
    guild: Value,
    channel: Value,
    attempt: Option<voice::JoinAttempt>,
) -> Socket {
    let mut req = format!("ws://{addr}{}", voice::VOICE_PATH)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    socket
        .send(Message::Ping(b"before-join".as_slice().into()))
        .await
        .unwrap();
    socket
        .send(Message::Pong(b"unsolicited".as_slice().into()))
        .await
        .unwrap();
    send(
        &mut socket,
        ClientEvent::Join {
            token: token.into(),
            guild_id: serde_json::from_value(guild).unwrap(),
            channel_id: serde_json::from_value(channel).unwrap(),
            attempt,
        },
    )
    .await;
    socket
}
struct Peer {
    socket: Socket,
    resume_token: String,
    pc: Arc<dyn PeerConnection>,
    track: Arc<TrackLocalStaticRTP>,
    packets: mpsc::Receiver<rtp::Packet>,
    slot: usize,
    sequence: u16,
    microphone_generation: Option<u64>,
    microphone_challenge: Option<u32>,
    microphone_satisfied: u32,
    screen_tracks: [Arc<TrackLocalStaticRTP>; 3],
    low_track: Option<Arc<TrackLocalStaticRTP>>,
    screen_sequences: [u16; 3],
    views: Vec<thiscord_shared::screen::Subscription>,
}
impl Peer {
    async fn new(socket: Socket) -> Self {
        Self::with_packet_capacity(socket, 64).await
    }
    async fn with_packet_capacity(socket: Socket, capacity: usize) -> Self {
        Self::with_red(socket, capacity, false).await
    }
    async fn with_red(socket: Socket, capacity: usize, packet_redundancy: bool) -> Self {
        Self::with_layers(socket, capacity, packet_redundancy, false).await
    }
    async fn with_layers(
        mut socket: Socket,
        capacity: usize,
        packet_redundancy: bool,
        screen_layers: bool,
    ) -> Self {
        let ServerEvent::Offer {
            sdp,
            slot,
            resume_token,
            ..
        } = event(&mut socket).await
        else {
            panic!("Expected offer")
        };
        socket
            .send(Message::Ping(b"before-answer".as_slice().into()))
            .await
            .unwrap();
        let (tx, rx) = mpsc::channel(capacity);
        let gathered = Arc::new(Notify::new());
        let mut media = MediaEngine::default();
        media.register_default_codecs().unwrap();
        if packet_redundancy {
            media
                .register_codec(
                    RTCRtpCodecParameters {
                        rtp_codec: RTCRtpCodec {
                            mime_type: thiscord_shared::rtp_red::MIME.into(),
                            clock_rate: 48_000,
                            channels: 2,
                            sdp_fmtp_line: thiscord_shared::rtp_red::FMTP.into(),
                            ..Default::default()
                        },
                        payload_type: thiscord_shared::rtp_red::PAYLOAD_TYPE,
                    },
                    RtpCodecKind::Audio,
                )
                .unwrap();
        }
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_media_engine(media)
                .with_handler(Arc::new(Handler {
                    gathered: gathered.clone(),
                    packets: tx,
                }))
                .with_udp_addrs(vec!["127.0.0.1:0".to_owned()])
                .build()
                .await
                .unwrap(),
        );
        pc.set_remote_description(serde_json::from_str(&sdp).unwrap())
            .await
            .unwrap();
        let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
            "test".into(),
            "microphone".into(),
            "test".into(),
            RtpCodecKind::Audio,
            vec![RTCRtpEncodingParameters {
                rtp_coding_parameters: RTCRtpCodingParameters {
                    ssrc: Some(900),
                    ..Default::default()
                },
                codec: RTCRtpCodec {
                    mime_type: "audio/opus".into(),
                    clock_rate: 48000,
                    channels: 2,
                    sdp_fmtp_line: "minptime=10;useinbandfec=1".into(),
                    ..Default::default()
                },
                ..Default::default()
            }],
        )));
        pc.add_track(track.clone() as Arc<dyn TrackLocal>)
            .await
            .unwrap();
        let screen_tracks = std::array::from_fn(|index| {
            let video = index != 1;
            Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
                format!("screen-{index}"),
                format!("screen-{index}"),
                format!("screen-{index}"),
                if video {
                    RtpCodecKind::Video
                } else {
                    RtpCodecKind::Audio
                },
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(901 + index as u32),
                        ..Default::default()
                    },
                    codec: RTCRtpCodec {
                        mime_type: if video { "video/H264" } else { "audio/opus" }.into(),
                        clock_rate: if video { 90000 } else { 48000 },
                        channels: if video { 0 } else { 2 },
                        sdp_fmtp_line: if video {
                            "level-asymmetry-allowed=1;packetization-mode=1;profile-level-id=42e01f"
                        } else {
                            "minptime=10;useinbandfec=1"
                        }
                        .into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
            )))
        });
        for track in &screen_tracks {
            pc.add_track(track.clone() as Arc<dyn TrackLocal>)
                .await
                .unwrap();
        }
        let low_track = if screen_layers {
            let track = Arc::new(TrackLocalStaticRTP::new(MediaStreamTrack::new(
                "screen-low".into(),
                "screen-low".into(),
                "screen-low".into(),
                RtpCodecKind::Video,
                vec![RTCRtpEncodingParameters {
                    rtp_coding_parameters: RTCRtpCodingParameters {
                        ssrc: Some(thiscord_shared::screen::LOW_LAYER_SSRC),
                        ..Default::default()
                    },
                    codec: RTCRtpCodec {
                        mime_type: "video/H264".into(),
                        clock_rate: 90_000,
                        sdp_fmtp_line: thiscord_shared::screen::H264_FMTP.into(),
                        ..Default::default()
                    },
                    ..Default::default()
                }],
            )));
            pc.add_track(track.clone() as Arc<dyn TrackLocal>)
                .await
                .unwrap();
            Some(track)
        } else {
            None
        };
        let answer = pc.create_answer(None).await.unwrap();
        if packet_redundancy {
            assert!(
                thiscord_shared::rtp_red::negotiated(&answer.sdp),
                "RED fmtp missing from negotiated answer"
            );
        }
        pc.set_local_description(answer).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), gathered.notified())
            .await
            .unwrap();
        send(
            &mut socket,
            ClientEvent::Answer {
                packet_redundancy,
                remote_audio: true,
                screen_feedback: true,
                screen_subscriptions: true,
                screen_layers,
                sdp: serde_json::to_string(&pc.local_description().await.unwrap()).unwrap(),
            },
        )
        .await;
        let mut microphone_challenge = None;
        if packet_redundancy {
            send(
                &mut socket,
                ClientEvent::MicrophoneBoundary { generation: 1 },
            )
            .await;
            loop {
                match event(&mut socket).await {
                    ServerEvent::MicrophoneBoundaryAccepted {
                        generation: 1,
                        accepted: true,
                        ..
                    } => break,
                    ServerEvent::MicrophoneRefreshRequired { challenge } => {
                        microphone_challenge = Some(challenge)
                    }
                    _ => {}
                }
            }
        }
        Self {
            socket,
            resume_token: resume_token.expect("resume capability"),
            pc,
            track,
            packets: rx,
            slot,
            sequence: 0,
            microphone_generation: packet_redundancy.then_some(1),
            microphone_challenge,
            microphone_satisfied: 0,
            screen_tracks,
            low_track,
            screen_sequences: [0; 3],
            views: vec![],
        }
    }
    async fn publish(&mut self) {
        self.sequence = self.sequence.wrapping_add(1);
        self.track
            .write_rtp(rtp::Packet {
                header: rtp::header::Header {
                    version: 2,
                    payload_type: 111,
                    ssrc: 900,
                    csrc: self
                        .microphone_generation
                        .map(|g| vec![(g >> 32) as u32, g as u32])
                        .unwrap_or_default(),
                    sequence_number: self.sequence,
                    timestamp: self.sequence as u32 * 960,
                    ..Default::default()
                },
                payload: vec![0xf8, 0xff, 0xfe].into(),
            })
            .await
            .unwrap();
    }
    async fn publish_screen(&mut self, kind: usize) {
        self.screen_sequences[kind] = self.screen_sequences[kind].wrapping_add(1);
        let sequence = self.screen_sequences[kind];
        self.screen_tracks[kind]
            .write_rtp(rtp::Packet {
                header: rtp::header::Header {
                    version: 2,
                    payload_type: if kind != 1 { 125 } else { 111 },
                    ssrc: 901 + kind as u32,
                    sequence_number: sequence,
                    timestamp: sequence as u32 * if kind != 1 { 9000 } else { 960 },
                    marker: true,
                    ..Default::default()
                },
                payload: if kind != 1 {
                    vec![0x65, 1, 2, 3]
                } else {
                    vec![0xf8, 0xff, 0xfe]
                }
                .into(),
            })
            .await
            .unwrap();
    }
    async fn close(mut self) {
        let _ = self.socket.close(None).await;
        self.pc.close().await.unwrap();
    }
}
async fn forwarded(source: &mut Peer, receiver: &mut Peer) -> rtp::Packet {
    while receiver.packets.try_recv().is_ok() {}
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            source.publish().await;
            if let Ok(Some(packet)) =
                tokio::time::timeout(Duration::from_millis(20), receiver.packets.recv()).await
            {
                return packet;
            }
        }
    })
    .await
    .expect("Authorized participants must still exchange media")
}
async fn watch_screen(source: &mut Peer, receiver: &mut Peer) {
    // Drain old rosters, then read the publisher's current share incarnation.
    still_connected(&mut source.socket).await;
    let member = loop {
        if let ServerEvent::Participants { members } = event(&mut source.socket).await
            && let Some(member) = members
                .into_iter()
                .find(|m| m.slot == source.slot && m.sharing_screen)
        {
            break member;
        }
    };
    receiver.views = vec![thiscord_shared::screen::Subscription {
        kind: thiscord_shared::voice::MediaKind::ScreenVideo,
        slot: member.slot,
        owner: member.account_id,
        epoch: member.screen_epoch,
        bitrate: 2_500_000,
    }];
    send(
        &mut receiver.socket,
        ClientEvent::ScreenViews {
            views: receiver.views.clone(),
        },
    )
    .await;
    still_connected(&mut receiver.socket).await;
}
async fn screen_forwarded(source: &mut Peer, receiver: &mut Peer, kind: usize) -> rtp::Packet {
    send(
        &mut receiver.socket,
        ClientEvent::ScreenViews {
            views: receiver.views.clone(),
        },
    )
    .await;
    while receiver.packets.try_recv().is_ok() {}
    let base = if kind == 0 {
        thiscord_shared::screen::SSRC_BASE
    } else if kind == 2 {
        voice::MediaKind::Camera.ssrc_base()
    } else {
        thiscord_shared::screen::AUDIO_SSRC_BASE
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            source.publish_screen(kind).await;
            if let Ok(Some(packet)) =
                tokio::time::timeout(Duration::from_millis(25), receiver.packets.recv()).await
                && packet.header.ssrc == base + source.slot as u32
            {
                return packet;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("Screen media kind {kind} must be forwarded"))
}
async fn screen_blocked(source: &mut Peer, receiver: &mut Peer) {
    tokio::time::sleep(Duration::from_millis(150)).await;
    while receiver.packets.try_recv().is_ok() {}
    for _ in 0..6 {
        source.publish_screen(0).await;
        source.publish_screen(1).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(40), receiver.packets.recv())
                .await
                .is_err(),
            "Stopped or unauthorized screen media was forwarded"
        );
    }
}

/// Transport load, not a codec benchmark: real authenticated peers send opaque
/// H.264-sized payloads through the production SFU while microphone RTP continues.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn sfu_sustains_authorized_4k60_and_limits_excess_egress() {
    let db = database();
    let (_, ot) = user(&db, "load_owner");
    let (_, mt) = user(&db, "load_viewer");
    permissions::bootstrap_owner(&db.pool, "load_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Media load"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"load_viewer"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app).into_future());
    let mut a = Peer::new(join(addr, &ot, guild.clone(), channel.clone()).await).await;
    let mut b = Peer::with_packet_capacity(join(addr, &mt, guild, channel).await, 4096).await;
    forwarded(&mut a, &mut b).await;
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: true,
            audio: false,
        },
    )
    .await;
    watch_screen(&mut a, &mut b).await;
    b.views[0].bitrate = thiscord_shared::screen::MAX_VIEW_BITRATE;
    screen_forwarded(&mut a, &mut b, 0).await;
    let mut sequence = a.screen_sequences[0];
    let mut microphone_sequence = a.sequence;
    for (fps, seconds, bitrate) in [
        (60_u32, 8_u32, thiscord_shared::screen::MAX_VIEW_BITRATE),
        (120, 8, thiscord_shared::screen::MAX_VIEW_BITRATE),
        (120, 2, thiscord_shared::screen::MIN_VIEW_BITRATE),
    ] {
        while b.packets.try_recv().is_ok() {}
        b.views[0].bitrate = bitrate;
        send(
            &mut b.socket,
            ClientEvent::ScreenViews {
                views: b.views.clone(),
            },
        )
        .await;
        still_connected(&mut b.socket).await;
        let frames = fps * seconds;
        let expected = frames as usize * 60;
        let video_ssrc = thiscord_shared::screen::SSRC_BASE + a.slot as u32;
        let audio_ssrc = voice::SSRC_BASE + a.slot as u32;
        let video = a.screen_tracks[0].clone();
        let microphone = a.track.clone();
        let started = tokio::time::Instant::now();
        let transmitting = tokio::spawn(async move {
            for frame in 0..frames {
                tokio::time::sleep_until(
                    started + Duration::from_secs_f64(f64::from(frame) / f64::from(fps)),
                )
                .await;
                for fragment in 0..60u32 {
                    sequence = sequence.wrapping_add(1);
                    let mut payload = vec![1u8; 1112];
                    payload[0] = 0x7c; // FU-A with explicit start/end boundaries.
                    payload[1] = 5 | if fragment == 0 {
                        0x80
                    } else if fragment == 59 {
                        0x40
                    } else {
                        0
                    };
                    payload[2..6].copy_from_slice(&(frame * 60 + fragment).to_be_bytes());
                    video
                        .write_rtp(rtp::Packet {
                            header: rtp::header::Header {
                                version: 2,
                                payload_type: 125,
                                ssrc: 901,
                                sequence_number: sequence,
                                timestamp: frame * (90000 / fps),
                                marker: fragment == 59,
                                ..Default::default()
                            },
                            payload: payload.into(),
                        })
                        .await
                        .unwrap();
                }
                if frame.is_multiple_of(fps / 20) {
                    microphone_sequence = microphone_sequence.wrapping_add(1);
                    microphone
                        .write_rtp(rtp::Packet {
                            header: rtp::header::Header {
                                version: 2,
                                payload_type: 111,
                                ssrc: 900,
                                sequence_number: microphone_sequence,
                                timestamp: u32::from(microphone_sequence) * 2400,
                                ..Default::default()
                            },
                            payload: vec![0xf8, 0xff, 0xfe].into(),
                        })
                        .await
                        .unwrap();
                }
            }
            (sequence, microphone_sequence, started.elapsed())
        });
        let mut seen = vec![false; expected];
        let mut received = 0;
        let mut audio = 0;
        let mut renew = tokio::time::interval(Duration::from_millis(500));
        let deadline = started + Duration::from_secs(u64::from(seconds) + 3);
        while received < expected {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => break,
                _ = renew.tick() => {
                    send(&mut b.socket, ClientEvent::ScreenViews { views: b.views.clone() }).await;
                    send(&mut a.socket, ClientEvent::Ping {}).await;
                },
                packet = b.packets.recv() => {
                    let packet = packet.expect("media peer closed");
                    if packet.header.ssrc == video_ssrc && packet.payload.len() == 1112 {
                        let index = u32::from_be_bytes(packet.payload[2..6].try_into().unwrap()) as usize;
                        assert!(index < expected);
                        assert!(!seen[index], "duplicate video packet");
                        seen[index] = true;
                        received += 1;
                    } else if packet.header.ssrc == audio_ssrc {
                        audio += 1;
                    }
                }
            }
        }
        let (last_sequence, last_microphone_sequence, elapsed) = transmitting.await.unwrap();
        sequence = last_sequence;
        microphone_sequence = last_microphone_sequence;
        println!(
            "SFU load: fps={fps}, Mbps={:.2}, received={received}/{expected}, microphone={audio}",
            expected as f64 * 1112.0 * 8.0 / elapsed.as_secs_f64() / 1e6
        );
        if fps == 60 {
            assert_eq!(
                received, expected,
                "authorized 4k60 must retain full delivery"
            );
        } else {
            // The old test demanded unrestricted 64 Mbit/s egress from a
            // 32 Mbit/s view. Encrypted transport must now enforce the actual
            // subscription, including a legacy publisher claiming no bitrate.
            let rate = f64::from(bitrate) * 1.2 / 8.0;
            let burst = (rate * 0.25).max(3000.0);
            // Payload + serialized relay RTP + accounted SRTP/UDP/IP overhead.
            let measured = received * (1112 + 16 + 64);
            assert!(received < expected / 4 * 3);
            if bitrate == thiscord_shared::screen::MAX_VIEW_BITRATE {
                assert!(received > 0);
            }
            assert!(
                measured as f64 <= rate * (elapsed.as_secs_f64() + 0.15) + burst,
                "{measured} measured bytes exceeded {bitrate} bit/s view"
            );
        }
        assert!(
            audio >= (seconds * 18) as usize,
            "microphone delivery must continue under video load"
        );
        assert!(
            elapsed < Duration::from_millis(u64::from(seconds) * 1000 + 200),
            "load generator did not sustain target rate"
        );
        if fps == 120 {
            // Deliberate over-budget bursts can drop whole frames. Once the
            // publisher reduces output, fresh encrypted media must resume even
            // if the locally dropped sequence gap exceeded the reorder window.
            a.screen_sequences[0] = sequence;
            tokio::time::sleep(Duration::from_millis(300)).await;
            screen_forwarded(&mut a, &mut b, 0).await;
            sequence = a.screen_sequences[0];
        }
    }
    a.close().await;
    b.close().await;
    serving.abort();
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn voice_takeover_fences_old_peers_and_delayed_retries() {
    let db = database();
    let (_, token) = user(&db, "takeover_owner");
    let (listener_id, listener_token) = user(&db, "takeover_listener");
    let (_, outsider) = user(&db, "takeover_outsider");
    permissions::bootstrap_owner(&db.pool, "takeover_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &token,
        json!({"action":"create_guild","name":"Takeover"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &token,
        &mut state,
        json!({"action":"add_member","username":"takeover_listener"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &token,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let guild = state["guild"]["id"].clone();
    let channel = state["channels"][0]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let call = voice::JoinAttempt {
        call_id: thiscord_shared::RequestId::from_uuid(Uuid::new_v4()),
        reconnect: false,
    };
    let mut old =
        Peer::new(join_attempt(addr, &token, guild.clone(), channel.clone(), Some(call)).await)
            .await;
    let mut witness =
        Peer::new(join(addr, &listener_token, guild.clone(), channel.clone()).await).await;
    forwarded(&mut old, &mut witness).await;
    // RFC 6455 control frames are legal during an active call. A protocol ping
    // is different from our JSON ClientEvent::Ping and must not terminate voice.
    old.socket
        .send(Message::Ping(b"keepalive".as_slice().into()))
        .await
        .unwrap();
    old.socket
        .send(Message::Pong(b"unsolicited".as_slice().into()))
        .await
        .unwrap();
    still_connected(&mut old.socket).await;
    forwarded(&mut old, &mut witness).await;
    // Cut TCP without sending Leave or closing WebRTC. Media must flow in
    // both directions before any replacement signaling has authenticated.
    let replacement = open_voice(addr).await;
    drop(std::mem::replace(&mut old.socket, replacement));
    tokio::time::sleep(Duration::from_millis(100)).await;
    forwarded(&mut old, &mut witness).await;
    forwarded(&mut witness, &mut old).await;
    resume(
        &mut old.socket,
        &token,
        &guild,
        &channel,
        &old.resume_token,
        1,
    )
    .await;
    assert!(matches!(
        event(&mut old.socket).await,
        ServerEvent::Resumed {}
    ));
    forwarded(&mut old, &mut witness).await;
    // A proxy can reset only the client leg while the original server socket
    // is still healthy. Reattachment must work in that case as well.
    let mut newer_socket = open_voice(addr).await;
    resume(
        &mut newer_socket,
        &token,
        &guild,
        &channel,
        &old.resume_token,
        2,
    )
    .await;
    assert!(matches!(
        event(&mut newer_socket).await,
        ServerEvent::Resumed {}
    ));
    old.socket = newer_socket;
    let mut replay = open_voice(addr).await;
    resume(&mut replay, &token, &guild, &channel, &old.resume_token, 1).await;
    assert!(
        matches!(event(&mut replay).await, ServerEvent::Error { error } if error.code == thiscord_shared::ErrorCode::Conflict)
    );
    let mut cross_account = open_voice(addr).await;
    resume(
        &mut cross_account,
        &listener_token,
        &guild,
        &channel,
        &old.resume_token,
        3,
    )
    .await;
    assert!(matches!(
        event(&mut cross_account).await,
        ServerEvent::Replaced {}
    ));
    forwarded(&mut old, &mut witness).await;
    still_connected(&mut old.socket).await;
    // A reconnect replaces its own live/stale socket immediately.
    let mut resumed = Peer::new(
        join_attempt(
            addr,
            &token,
            guild.clone(),
            channel.clone(),
            Some(voice::JoinAttempt {
                reconnect: true,
                ..call
            }),
        )
        .await,
    )
    .await;
    assert_eq!(old.slot, resumed.slot);
    expect_replaced(&mut old.socket).await;
    let mut stale_resume = open_voice(addr).await;
    resume(
        &mut stale_resume,
        &token,
        &guild,
        &channel,
        &old.resume_token,
        3,
    )
    .await;
    assert!(matches!(
        event(&mut stale_resume).await,
        ServerEvent::Replaced {}
    ));
    old.close().await;
    forwarded(&mut resumed, &mut witness).await;
    // A fresh explicit join takes ownership; the displaced caller's delayed
    // automatic retries must not start a takeover loop.
    let new_call = voice::JoinAttempt {
        call_id: thiscord_shared::RequestId::from_uuid(Uuid::new_v4()),
        reconnect: false,
    };
    let mut newest =
        Peer::new(join_attempt(addr, &token, guild.clone(), channel.clone(), Some(new_call)).await)
            .await;
    expect_replaced(&mut resumed.socket).await;
    resumed.close().await;
    let mut stale = join_attempt(
        addr,
        &token,
        guild.clone(),
        channel.clone(),
        Some(voice::JoinAttempt {
            reconnect: true,
            ..call
        }),
    )
    .await;
    expect_replaced(&mut stale).await;
    let mut denied =
        join_attempt(addr, &outsider, guild.clone(), channel.clone(), Some(call)).await;
    assert!(matches!(
        event(&mut denied).await,
        ServerEvent::Error { .. }
    ));
    still_connected(&mut newest.socket).await;
    forwarded(&mut newest, &mut witness).await;
    // Replacement also interrupts a peer waiting for its SDP answer.
    let mut pending = join_attempt(addr, &token, guild.clone(), channel.clone(), Some(call)).await;
    assert!(matches!(
        event(&mut pending).await,
        ServerEvent::Offer { .. }
    ));
    expect_replaced(&mut newest.socket).await;
    newest.close().await;
    let mut final_peer =
        Peer::new(join_attempt(addr, &token, guild.clone(), channel.clone(), Some(new_call)).await)
            .await;
    expect_replaced(&mut pending).await;
    still_connected(&mut final_peer.socket).await;
    forwarded(&mut final_peer, &mut witness).await;
    // Revocation during the detached grace period still fences media and
    // refuses the capability even though the original peer remains allocated.
    let replacement = open_voice(addr).await;
    drop(std::mem::replace(&mut witness.socket, replacement));
    change(
        &app,
        &token,
        &mut state,
        json!({"action":"ban_member","account_id":listener_id}),
        StatusCode::OK,
    )
    .await;
    resume(
        &mut witness.socket,
        &listener_token,
        &guild,
        &channel,
        &witness.resume_token,
        1,
    )
    .await;
    assert!(matches!(
        event(&mut witness.socket).await,
        ServerEvent::Error { .. }
    ));
    while final_peer.packets.try_recv().is_ok() {}
    for _ in 0..5 {
        witness.publish().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(40), final_peer.packets.recv())
                .await
                .is_err()
        );
    }
    // A detached session has a bounded lifetime, even when its ICE peer is
    // otherwise healthy. Expiry must not allocate a peer during Resume.
    let replacement = open_voice(addr).await;
    drop(std::mem::replace(&mut final_peer.socket, replacement));
    tokio::time::sleep(Duration::from_secs(voice::SIGNALING_GRACE_SECS + 2)).await;
    final_peer.socket = open_voice(addr).await;
    resume(
        &mut final_peer.socket,
        &token,
        &guild,
        &channel,
        &final_peer.resume_token,
        1,
    )
    .await;
    assert!(matches!(
        event(&mut final_peer.socket).await,
        ServerEvent::ResumeUnavailable {}
    ));
    final_peer.close().await;
    witness.close().await;
    serving.abort();
    let _ = serving.await;
}

async fn open_voice(addr: std::net::SocketAddr) -> Socket {
    let mut request = format!("ws://{addr}{}", voice::VOICE_PATH)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    tokio_tungstenite::connect_async(request).await.unwrap().0
}
async fn resume(
    socket: &mut Socket,
    token: &str,
    guild: &Value,
    channel: &Value,
    secret: &str,
    sequence: u64,
) {
    send(
        socket,
        ClientEvent::Resume {
            token: token.into(),
            guild_id: serde_json::from_value(guild.clone()).unwrap(),
            channel_id: serde_json::from_value(channel.clone()).unwrap(),
            resume_token: secret.into(),
            sequence,
        },
    )
    .await;
}

async fn expect_replaced(socket: &mut Socket) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match event(socket).await {
                ServerEvent::Replaced {} => break,
                ServerEvent::Participants { .. } | ServerEvent::MediaDiagnostics { .. } | ServerEvent::ScreenTarget { .. } => {},
                _ => panic!("Expected terminal replacement notification"),
            }
        }
        let message = socket.next().await.unwrap().unwrap();
        assert!(matches!(message, Message::Close(Some(frame)) if u16::from(frame.code) == voice::REPLACED_CLOSE_CODE));
    }).await.expect("Takeover must not wait for heartbeat or negotiation timeout");
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn sfu_forwarding_permissions_isolation_and_cleanup() {
    let db = database();
    let (owner, ot) = user(&db, "voice_owner");
    let (member, mt) = user(&db, "voice_member");
    let (_, outsider) = user(&db, "voice_outsider");
    permissions::bootstrap_owner(&db.pool, "voice_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Voice test"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"voice_member"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"other","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let other = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "other")
        .unwrap()["id"]
        .clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut denied = join(addr, &outsider, guild.clone(), channel.clone()).await;
    assert!(matches!(
        event(&mut denied).await,
        ServerEvent::Error { .. }
    ));
    let mut a = Peer::new(join(addr, &ot, guild.clone(), channel.clone()).await).await;
    let mut b = Peer::new(join(addr, &mt, guild.clone(), channel.clone()).await).await;
    let mut isolated = Peer::new(join(addr, &ot, guild.clone(), other).await).await;
    camera_grid(&mut a, &mut b).await;
    assert!(isolated.packets.try_recv().is_err());
    // Existing peers receive profile picture changes/removal without renegotiation.
    let mut picture = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(8, 8)
        .write_to(&mut picture, image::ImageFormat::Png)
        .unwrap();
    let picture = base64::engine::general_purpose::STANDARD.encode(picture.into_inner());
    for picture in [Some(picture.clone()), Some(picture), None] {
        let (status, response) = call(
            &app,
            ACCOUNT_PATH,
            &ot,
            json!({"action":"set_avatar","image_base64":picture}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let expected: Option<thiscord_shared::AvatarId> =
            serde_json::from_value(response["account"]["avatar_id"].clone()).unwrap();
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let ServerEvent::Participants { members } = event(&mut b.socket).await
                    && members
                        .iter()
                        .any(|m| m.account_id == owner && m.avatar_id == expected)
                {
                    break;
                }
            }
        })
        .await
        .expect("A connected peer must see current profile pictures");
    }
    let packet = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            a.publish().await;
            if let Ok(packet) =
                tokio::time::timeout(Duration::from_millis(20), b.packets.recv()).await
            {
                return packet.unwrap();
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(packet.header.ssrc, voice::SSRC_BASE + a.slot as u32);
    assert_eq!(packet.payload.as_ref(), [0xf8, 0xff, 0xfe]);
    assert!(a.packets.try_recv().is_err());
    assert!(isolated.packets.try_recv().is_err());
    // Admin diagnostics read the actual live registry, including multiple rooms.
    let diagnostic_snapshot = tokio::time::timeout(Duration::from_secs(12), async {
        loop {
            a.publish().await;
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(thiscord_shared::admin::DIAGNOSTICS_PATH)
                        .header("authorization", format!("Bearer {ot}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let snapshot: thiscord_shared::admin::Diagnostics =
                serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap())
                    .unwrap();
            assert_eq!(snapshot.rooms.len(), 2);
            assert_eq!(
                snapshot
                    .rooms
                    .iter()
                    .map(|r| r.participants.len())
                    .sum::<usize>(),
                3
            );
            let room = snapshot
                .rooms
                .iter()
                .find(|r| json!(r.channel_id) == channel)
                .unwrap();
            let publisher = room
                .participants
                .iter()
                .find(|p| p.participant.account_id == owner)
                .unwrap();
            if publisher
                .network
                .microphone_packets_received
                .is_some_and(|n| n > 0)
            {
                assert!(
                    publisher
                        .network
                        .round_trip_ms
                        .is_some_and(|n| n.is_finite() && n >= 0.0)
                );
                assert!(
                    publisher
                        .network
                        .microphone_jitter_ms
                        .is_some_and(|n| n.is_finite() && n >= 0.0)
                );
                break snapshot;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    if cfg!(target_os = "linux") {
        assert!(diagnostic_snapshot.host.memory_total_bytes.is_some());
    }
    // Remote control is delivered to the exact live connection and accepts replies only there.
    let audio_connection = diagnostic_snapshot
        .rooms
        .iter()
        .flat_map(|r| &r.participants)
        .find(|p| p.participant.account_id == member)
        .unwrap()
        .audio_connection_id
        .unwrap();
    let request_app = app.clone();
    let admin_token = ot.clone();
    let mut request = tokio::spawn(async move {
        call(
            &request_app,
            thiscord_shared::remote_audio::PATH,
            &admin_token,
            json!({"connection_id":audio_connection,"command":{"command":"audio_current"}}),
        )
        .await
    });
    let request_id = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let ServerEvent::AudioCommand {
                request_id,
                command,
            } = event(&mut b.socket).await
            {
                assert!(matches!(
                    command,
                    thiscord_shared::remote_audio::Command::AudioCurrent {}
                ));
                break request_id;
            }
        }
    })
    .await
    .unwrap();
    send(
        &mut isolated.socket,
        ClientEvent::AudioReply {
            request_id,
            chunk: r#"{"Ok":"forged"}"#.into(),
            done: true,
        },
    )
    .await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut request)
            .await
            .is_err(),
        "another connection cannot supply the response"
    );
    send(
        &mut b.socket,
        ClientEvent::AudioReply {
            request_id,
            chunk: r#"{"Ok":{"output_"#.into(),
            done: false,
        },
    )
    .await;
    send(
        &mut b.socket,
        ClientEvent::AudioReply {
            request_id,
            chunk: r#"volume":0.5}}"#.into(),
            done: true,
        },
    )
    .await;
    let (status, value) = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status, StatusCode::OK);
    assert_eq!(value["Ok"]["output_volume"], 0.5);
    // Signaling recovery preserves the admin connection identity, but an
    // interrupted chunked response must fail instead of returning partial JSON.
    for interrupted in [true, false] {
        let request_app = app.clone();
        let admin_token = ot.clone();
        let request = tokio::spawn(async move {
            call(
                &request_app,
                thiscord_shared::remote_audio::PATH,
                &admin_token,
                json!({"connection_id":audio_connection,"command":{"command":"audio_current"}}),
            )
            .await
        });
        let request_id = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let ServerEvent::AudioCommand { request_id, .. } = event(&mut b.socket).await {
                    break request_id;
                }
            }
        })
        .await
        .unwrap();
        if interrupted {
            send(
                &mut b.socket,
                ClientEvent::AudioReply {
                    request_id,
                    chunk: r#"{"Ok":{"output_"#.into(),
                    done: false,
                },
            )
            .await;
            still_connected(&mut b.socket).await;
            let replacement = open_voice(addr).await;
            drop(std::mem::replace(&mut b.socket, replacement));
            forwarded(&mut a, &mut b).await;
            resume(&mut b.socket, &mt, &guild, &channel, &b.resume_token, 1).await;
            assert!(matches!(
                event(&mut b.socket).await,
                ServerEvent::Resumed {}
            ));
        } else {
            send(
                &mut b.socket,
                ClientEvent::AudioReply {
                    request_id,
                    chunk: r#"{"Ok":{"output_volume":0.75}}"#.into(),
                    done: true,
                },
            )
            .await;
        }
        let (status, value) = tokio::time::timeout(Duration::from_secs(5), request)
            .await
            .unwrap()
            .unwrap();
        if interrupted {
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        } else {
            assert_eq!(status, StatusCode::OK);
            assert_eq!(value["Ok"]["output_volume"], 0.75);
        }
    }
    // Screen publishing requires explicit state, even for an authorized speaker.
    screen_blocked(&mut a, &mut b).await;
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: true,
            audio: true,
        },
    )
    .await;
    still_connected(&mut a.socket).await;
    screen_blocked(&mut a, &mut b).await; // Room membership alone is not a viewer.
    watch_screen(&mut a, &mut b).await;
    let video = screen_forwarded(&mut a, &mut b, 0).await;
    assert_eq!(video.payload.as_ref(), [0x65, 1, 2, 3]);
    assert!(video.header.marker);
    assert_eq!(video.header.csrc.len(), 1);
    // Authenticated receiver feedback is routed to the current publisher epoch.
    send(
        &mut b.socket,
        ClientEvent::ScreenKeyframe {
            kind: thiscord_shared::voice::MediaKind::ScreenVideo,
            slot: a.slot,
            epoch: video.header.csrc[0],
        },
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            match event(&mut a.socket).await {
                ServerEvent::ScreenKeyframe { epoch, .. } => {
                    assert_eq!(epoch, video.header.csrc[0]);
                    break;
                }
                ServerEvent::Participants { .. }
                | ServerEvent::MediaDiagnostics { .. }
                | ServerEvent::ScreenTarget { .. } => {}
                _ => panic!("Unexpected feedback event"),
            }
        }
    })
    .await
    .expect("keyframe feedback must not wait for the next periodic IDR");

    assert_eq!(
        screen_forwarded(&mut a, &mut b, 1).await.payload.as_ref(),
        [0xf8, 0xff, 0xfe]
    );
    assert!(isolated.packets.try_recv().is_err());
    send(&mut b.socket, ClientEvent::ScreenViews { views: vec![] }).await;
    still_connected(&mut b.socket).await;
    screen_blocked(&mut a, &mut b).await;
    forwarded(&mut a, &mut b).await; // Microphone remains independent.
    watch_screen(&mut a, &mut b).await;
    send(
        &mut a.socket,
        ClientEvent::State {
            muted: true,
            deafened: false,
        },
    )
    .await;
    still_connected(&mut a.socket).await;
    screen_forwarded(&mut a, &mut b, 0).await;
    screen_forwarded(&mut a, &mut b, 1).await;
    send(
        &mut b.socket,
        ClientEvent::State {
            muted: false,
            deafened: true,
        },
    )
    .await;
    still_connected(&mut b.socket).await;
    screen_blocked(&mut a, &mut b).await;
    send(
        &mut b.socket,
        ClientEvent::State {
            muted: false,
            deafened: false,
        },
    )
    .await;
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: false,
            audio: false,
        },
    )
    .await;
    still_connected(&mut a.socket).await;
    screen_blocked(&mut a, &mut b).await;
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: true,
            audio: true,
        },
    )
    .await;
    watch_screen(&mut a, &mut b).await;
    assert!(
        screen_forwarded(&mut a, &mut b, 0)
            .await
            .header
            .sequence_number
            .wrapping_sub(video.header.sequence_number) as i16
            > 0
    );
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: false,
            audio: false,
        },
    )
    .await;
    send(
        &mut a.socket,
        ClientEvent::State {
            muted: false,
            deafened: false,
        },
    )
    .await;
    still_connected(&mut a.socket).await;
    while b.packets.try_recv().is_ok() {}
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: true,
            audio: true,
        },
    )
    .await;
    still_connected(&mut a.socket).await;
    watch_screen(&mut a, &mut b).await;
    // Hold the guild row without changing access. Wait until a periodic voice
    // check actually blocks in PostgreSQL, then require media while it is held.
    // Previously that check held the global write gate and stalled every stream.
    let mut blocker = db.pool.get().unwrap();
    blocker.batch_execute("BEGIN").unwrap();
    diesel::sql_query("SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE")
        .bind::<Text, _>(guild.as_str().unwrap())
        .execute(&mut blocker)
        .unwrap();
    #[derive(QueryableByName)]
    struct Waiting {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        waiting: bool,
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: Waiting = diesel::sql_query(
                "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE pg_backend_pid() = ANY(pg_blocking_pids(pid))) AS waiting",
            )
            .get_result(&mut blocker)
            .unwrap();
            if waiting.waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("A periodic voice query must reach the held guild lock");
    let during_check = tokio::time::timeout(Duration::from_millis(350), async {
        forwarded(&mut a, &mut b).await;
        screen_forwarded(&mut a, &mut b, 0).await;
        screen_forwarded(&mut a, &mut b, 1).await;
    })
    .await;
    blocker.batch_execute("ROLLBACK").unwrap();
    drop(blocker);
    during_check
        .expect("A blocked authorization query must not stall voice, screen video or shared audio");
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: false,
            audio: false,
        },
    )
    .await;
    still_connected(&mut a.socket).await;
    while b.packets.try_recv().is_ok() {}
    // A session response for another account must not disconnect active calls.
    let (status, _) = call(&app, ACCOUNT_PATH, &outsider, json!({"action":"rotate"})).await;
    assert_eq!(status, StatusCode::OK);
    still_connected(&mut a.socket).await;
    still_connected(&mut b.socket).await;
    still_connected(&mut isolated.socket).await;
    forwarded(&mut a, &mut b).await;
    forwarded(&mut b, &mut a).await;
    // A guild mutation that does not change either participant's media grants
    // must also keep both existing forwarding tasks alive.
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"unrelated","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    still_connected(&mut a.socket).await;
    still_connected(&mut b.socket).await;
    let old_sequence = forwarded(&mut a, &mut b).await.header.sequence_number;
    a.close().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    while b.packets.try_recv().is_ok() {}
    let mut a = Peer::new(join(addr, &ot, guild.clone(), channel.clone()).await).await;
    assert_eq!(a.slot, 0);
    let replacement = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            a.publish().await;
            if let Ok(Some(packet)) =
                tokio::time::timeout(Duration::from_millis(20), b.packets.recv()).await
            {
                return packet;
            }
        }
    })
    .await
    .unwrap();
    assert!(
        replacement
            .header
            .sequence_number
            .wrapping_sub(old_sequence) as i16
            > 0,
        "A reused publisher slot must preserve SRTP sequence continuity"
    );

    send(
        &mut b.socket,
        ClientEvent::Screen {
            quality: None,
            active: true,
            audio: true,
        },
    )
    .await;
    watch_screen(&mut b, &mut a).await;
    screen_forwarded(&mut b, &mut a, 0).await;
    screen_forwarded(&mut b, &mut a, 1).await;
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: true,
            audio: false,
        },
    )
    .await;
    watch_screen(&mut a, &mut b).await;
    let reused_video = screen_forwarded(&mut a, &mut b, 0).await;
    assert_ne!(
        video.header.csrc, reused_video.header.csrc,
        "Reused slots must have a new screen epoch"
    );
    send(
        &mut a.socket,
        ClientEvent::Screen {
            quality: None,
            active: false,
            audio: false,
        },
    )
    .await;
    // A moderator disconnect revokes only the target's current transports.
    // It stops screen media too, while permitting a later explicit join.
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"disconnect_voice","account_id":member}),
        StatusCode::OK,
    )
    .await;
    loop {
        if matches!(event(&mut b.socket).await, ServerEvent::Revoked {}) {
            break;
        }
    }
    still_connected(&mut a.socket).await;
    still_connected(&mut isolated.socket).await;
    screen_blocked(&mut b, &mut a).await;
    b.close().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    let mut b = Peer::new(join(addr, &mt, guild.clone(), channel.clone()).await).await;
    forwarded(&mut a, &mut b).await;
    for action in ["timeout_member", "ban_member"] {
        let change_request = if action == "timeout_member" {
            json!({"action":action,"account_id":member,"duration_seconds":60})
        } else {
            json!({"action":action,"account_id":member})
        };
        change(&app, &ot, &mut state, change_request, StatusCode::OK).await;
        loop {
            if matches!(event(&mut b.socket).await, ServerEvent::Revoked {}) {
                break;
            }
        }
        still_connected(&mut a.socket).await;
        screen_blocked(&mut b, &mut a).await;
        b.close().await;
        let mut denied = join(addr, &mt, guild.clone(), channel.clone()).await;
        assert!(matches!(
            event(&mut denied).await,
            ServerEvent::Error { .. }
        ));
        if action == "timeout_member" {
            change(
                &app,
                &ot,
                &mut state,
                json!({"action":"timeout_member","account_id":member,"duration_seconds":null}),
                StatusCode::OK,
            )
            .await;
        } else {
            change(
                &app,
                &ot,
                &mut state,
                json!({"action":"unban_member","account_id":member}),
                StatusCode::OK,
            )
            .await;
            change(
                &app,
                &ot,
                &mut state,
                json!({"action":"add_member","username":"voice_member"}),
                StatusCode::OK,
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        b = Peer::new(join(addr, &mt, guild.clone(), channel.clone()).await).await;
        forwarded(&mut a, &mut b).await;
    }
    // Speak changes require renegotiating the native microphone setup, even
    // when the participant still has JoinVoice. Other participants stay joined.
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":["speak"]}),StatusCode::OK).await;
    loop {
        if matches!(event(&mut b.socket).await, ServerEvent::Revoked {}) {
            break;
        }
    }
    still_connected(&mut a.socket).await;
    still_connected(&mut isolated.socket).await;
    screen_blocked(&mut b, &mut a).await;
    b.close().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":[]}),StatusCode::OK).await;
    let mut b = Peer::new(join(addr, &mt, guild.clone(), channel.clone()).await).await;
    forwarded(&mut a, &mut b).await;

    send(
        &mut b.socket,
        ClientEvent::State {
            muted: false,
            deafened: true,
        },
    )
    .await;
    loop {
        if let ServerEvent::Participants { members } = event(&mut b.socket).await
            && members.iter().any(|m| m.account_id == member && m.deafened)
        {
            break;
        }
    }
    while b.packets.try_recv().is_ok() {}
    for _ in 0..5 {
        a.publish().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(b.packets.try_recv().is_err());
    send(
        &mut b.socket,
        ClientEvent::State {
            muted: false,
            deafened: false,
        },
    )
    .await;
    loop {
        if let ServerEvent::Participants { members } = event(&mut b.socket).await
            && members
                .iter()
                .any(|m| m.account_id == member && !m.deafened)
        {
            break;
        }
    }
    send(
        &mut a.socket,
        ClientEvent::State {
            muted: true,
            deafened: false,
        },
    )
    .await;
    loop {
        if let ServerEvent::Participants { members } = event(&mut a.socket).await
            && members.iter().any(|m| m.account_id == owner && m.muted)
        {
            break;
        }
    }
    while b.packets.try_recv().is_ok() {}
    for _ in 0..5 {
        a.publish().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        b.packets.try_recv().is_err(),
        "Muted publishers must not be forwarded"
    );
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":["join_voice"]}),StatusCode::OK).await;
    loop {
        if matches!(event(&mut b.socket).await, ServerEvent::Revoked {}) {
            break;
        }
    }
    let mut denied = join(addr, &mt, guild.clone(), channel.clone()).await;
    assert!(matches!(
        event(&mut denied).await,
        ServerEvent::Error { .. }
    ));
    still_connected(&mut a.socket).await;
    still_connected(&mut isolated.socket).await;
    a.close().await;
    b.close().await;
    isolated.close().await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":["speak"]}),StatusCode::OK).await;
    let mut c = Peer::new(join(addr, &ot, guild.clone(), channel.clone()).await).await;
    assert_eq!(c.slot, 0);
    let mut listener_only = Peer::new(join(addr, &mt, guild.clone(), channel.clone()).await).await;
    screen_blocked(&mut listener_only, &mut c).await;
    // Confirm the transport works in the permitted direction first.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            c.publish().await;
            if tokio::time::timeout(Duration::from_millis(20), listener_only.packets.recv())
                .await
                .is_ok()
            {
                break;
            }
        }
    })
    .await
    .unwrap();
    for _ in 0..5 {
        listener_only.publish().await;
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        c.packets.try_recv().is_err(),
        "JoinVoice must not grant Speak"
    );

    let (status, _) = call(&app, ACCOUNT_PATH, &mt, json!({"action":"logout"})).await;
    assert_eq!(status, StatusCode::OK);
    loop {
        if matches!(
            event(&mut listener_only.socket).await,
            ServerEvent::Revoked {}
        ) {
            break;
        }
    }
    still_connected(&mut c.socket).await;

    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"delete_channel","channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    loop {
        if matches!(event(&mut c.socket).await, ServerEvent::Revoked {}) {
            break;
        }
    }
    let mut deleted = join(addr, &ot, guild.clone(), channel.clone()).await;
    assert!(matches!(
        event(&mut deleted).await,
        ServerEvent::Error { .. }
    ));
    c.close().await;
    listener_only.close().await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(thiscord_shared::admin::DIAGNOSTICS_PATH)
                        .header("authorization", format!("Bearer {ot}"))
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let snapshot: thiscord_shared::admin::Diagnostics =
                serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap())
                    .unwrap();
            if snapshot.rooms.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(state["guild"]["owner"], json!(owner));
    serving.abort();
}

// Synthetic encoded packets exercise the real encrypted SFU without a camera.
async fn camera_roster(peer: &mut Peer) -> voice::Participant {
    still_connected(&mut peer.socket).await;
    loop {
        if let ServerEvent::Participants { members } = event(&mut peer.socket).await
            && let Some(m) = members
                .into_iter()
                .find(|m| m.slot == peer.slot && m.camera_active && m.sharing_screen)
        {
            return m;
        }
    }
}
async fn expect_viewers(
    source: &mut Peer,
    viewer: &mut Peer,
    publisher: &voice::Participant,
    watcher: &voice::Participant,
    screen: bool,
    camera: bool,
) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            send(
                &mut viewer.socket,
                ClientEvent::ScreenViews {
                    views: viewer.views.clone(),
                },
            )
            .await;
            if let ServerEvent::Participants { members } = event(&mut source.socket).await
                && let Some(info) = members
                    .iter()
                    .find(|m| m.account_id == publisher.account_id)
                && info.screen_viewers
                    == if screen {
                        vec![watcher.account_id]
                    } else {
                        vec![]
                    }
                && info.camera_viewers
                    == if camera {
                        vec![watcher.account_id]
                    } else {
                        vec![]
                    }
            {
                break;
            }
        }
    })
    .await
    .expect("Call participant snapshots must report current per-stream viewers");
}
async fn camera_grid(source: &mut Peer, viewer: &mut Peer) {
    use voice::MediaKind;
    for peer in [&mut *source, &mut *viewer] {
        send(&mut peer.socket, ClientEvent::Camera { active: true }).await;
        send(
            &mut peer.socket,
            ClientEvent::Screen {
                quality: None,
                active: true,
                audio: true,
            },
        )
        .await;
    }
    let a = camera_roster(source).await;
    let b = camera_roster(viewer).await;
    for (peer, publisher) in [(&mut *viewer, &a), (&mut *source, &b)] {
        peer.views = [MediaKind::ScreenVideo, MediaKind::Camera]
            .map(|kind| thiscord_shared::screen::Subscription {
                kind,
                slot: publisher.slot,
                owner: publisher.account_id,
                epoch: publisher.video_epoch(kind),
                bitrate: 1_500_000,
            })
            .to_vec();
    }
    for kind in 0..3 {
        let packet = screen_forwarded(source, viewer, kind).await;
        assert_eq!(
            packet.header.csrc,
            vec![if kind == 2 {
                a.camera_epoch
            } else {
                a.screen_epoch
            }]
        );
        screen_forwarded(viewer, source, kind).await;
    }
    expect_viewers(source, viewer, &a, &b, true, true).await;
    // Hiding a screen removes its audio, independently of the same owner's camera.
    viewer.views.retain(|v| v.kind == MediaKind::Camera);
    screen_forwarded(source, viewer, 2).await;
    screen_blocked(source, viewer).await;
    forwarded(source, viewer).await;
    expect_viewers(source, viewer, &a, &b, false, true).await;
    // A restarted camera cannot inherit an old subscription.
    send(&mut source.socket, ClientEvent::Camera { active: false }).await;
    send(&mut source.socket, ClientEvent::Camera { active: true }).await;
    let restarted = camera_roster(source).await;
    assert_ne!(restarted.camera_epoch, a.camera_epoch);
    assert_eq!(restarted.screen_epoch, a.screen_epoch);
    send(
        &mut viewer.socket,
        ClientEvent::ScreenViews {
            views: viewer.views.clone(),
        },
    )
    .await;
    still_connected(&mut viewer.socket).await;
    tokio::time::sleep(Duration::from_millis(200)).await;
    while viewer.packets.try_recv().is_ok() {}
    for _ in 0..5 {
        source.publish_screen(2).await;
        assert!(
            tokio::time::timeout(Duration::from_millis(40), viewer.packets.recv())
                .await
                .is_err()
        );
    }
    expect_viewers(source, viewer, &restarted, &b, false, false).await;
    viewer.views[0].epoch = restarted.camera_epoch;
    screen_forwarded(source, viewer, 2).await;
    for peer in [source, viewer] {
        send(&mut peer.socket, ClientEvent::Camera { active: false }).await;
        send(
            &mut peer.socket,
            ClientEvent::Screen {
                quality: None,
                active: false,
                audio: false,
            },
        )
        .await;
        peer.views.clear();
        send(&mut peer.socket, ClientEvent::ScreenViews { views: vec![] }).await;
        still_connected(&mut peer.socket).await;
        while peer.packets.try_recv().is_ok() {}
    }
}
