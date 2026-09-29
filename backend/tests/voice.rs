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

struct Database {
    connection: PgConnection,
    schema: String,
    pool: db::DbPool,
}
impl Drop for Database {
    fn drop(&mut self) {
        self.connection
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .unwrap();
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
    let frame = tokio::time::timeout(Duration::from_secs(20), socket.next())
        .await
        .unwrap()
        .unwrap()
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
            ServerEvent::Participants { .. } => {}
            ServerEvent::Revoked {} => panic!("Authorized voice connection was revoked"),
            _ => panic!("Unexpected event on an authorized voice connection"),
        }
    }
}
async fn join(addr: std::net::SocketAddr, token: &str, guild: Value, channel: Value) -> Socket {
    let mut req = format!("ws://{addr}{}", voice::VOICE_PATH)
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    send(
        &mut socket,
        ClientEvent::Join {
            token: token.into(),
            guild_id: serde_json::from_value(guild).unwrap(),
            channel_id: serde_json::from_value(channel).unwrap(),
        },
    )
    .await;
    socket
}
struct Peer {
    socket: Socket,
    pc: Arc<dyn PeerConnection>,
    track: Arc<TrackLocalStaticRTP>,
    packets: mpsc::Receiver<rtp::Packet>,
    slot: usize,
    sequence: u16,
}
impl Peer {
    async fn new(mut socket: Socket) -> Self {
        let ServerEvent::Offer { sdp, slot, .. } = event(&mut socket).await else {
            panic!("Expected offer")
        };
        let (tx, rx) = mpsc::channel(64);
        let gathered = Arc::new(Notify::new());
        let mut media = MediaEngine::default();
        media.register_default_codecs().unwrap();
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
        let answer = pc.create_answer(None).await.unwrap();
        pc.set_local_description(answer).await.unwrap();
        tokio::time::timeout(Duration::from_secs(5), gathered.notified())
            .await
            .unwrap();
        send(
            &mut socket,
            ClientEvent::Answer {
                sdp: serde_json::to_string(&pc.local_description().await.unwrap()).unwrap(),
            },
        )
        .await;
        Self {
            socket,
            pc,
            track,
            packets: rx,
            slot,
            sequence: 0,
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
                    sequence_number: self.sequence,
                    timestamp: self.sequence as u32 * 960,
                    ..Default::default()
                },
                payload: vec![0xf8, 0xff, 0xfe].into(),
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
    assert_eq!(state["guild"]["owner"], json!(owner));
    serving.abort();
}
