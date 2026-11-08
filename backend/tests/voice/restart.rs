//! Planned restarts drain voice and chat sockets with a notice; rejoins pass
//! fresh admission; ICE restarts keep the media session. Runs under whichever
//! SFU transport the suite selects (CI runs both).
use super::*;
use thiscord_backend::{
    limits::Limits,
    shutdown::{RETRY_BASE_MS, RETRY_JITTER_MS, Shutdown},
};
use thiscord_shared::{ErrorCode, RequestId, chat};

struct Fixture {
    db: Database,
    owner: String,
    member: String,
    member_id: AccountId,
    state: Value,
    guild: Value,
    channel: Value,
}

fn origins() -> Vec<axum::http::HeaderValue> {
    vec!["http://localhost:1420".parse().unwrap()]
}

async fn fixture(prefix: &str) -> Fixture {
    let db = database();
    let owner_name = format!("{prefix}_owner");
    let member_name = format!("{prefix}_member");
    let (_, owner) = user(&db, &owner_name);
    let (member_id, member) = user(&db, &member_name);
    permissions::bootstrap_owner(&db.pool, &owner_name).unwrap();
    let app = api::router(Some(db.pool.clone()), origins());
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Restart"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"add_member","username":member_name}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"voice","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let guild = state["guild"]["id"].clone();
    let channel = state["channels"][0]["id"].clone();
    Fixture {
        db,
        owner,
        member,
        member_id,
        state,
        guild,
        channel,
    }
}

async fn listen(app: Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    (addr, serving)
}

/// Reads until the server closes the socket: the text frames seen and the close code.
async fn until_close(socket: &mut Socket) -> (Vec<String>, Option<u16>) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut texts = vec![];
        loop {
            match socket.next().await {
                Some(Ok(Message::Text(text))) => texts.push(text.to_string()),
                Some(Ok(Message::Close(frame))) => {
                    return (texts, frame.map(|frame| u16::from(frame.code)));
                }
                Some(Ok(_)) => {}
                _ => return (texts, None),
            }
        }
    })
    .await
    .expect("a restart must close the socket promptly")
}

fn voice_notice(texts: &[String]) -> Option<u64> {
    texts.iter().find_map(
        |text| match serde_json::from_str::<ServerFrame>(text).ok()?.event {
            ServerEvent::Restarting { retry_after_ms } => Some(retry_after_ms),
            _ => None,
        },
    )
}

fn chat_notice(texts: &[String]) -> Option<u64> {
    texts.iter().find_map(|text| {
        match serde_json::from_str::<chat::ServerFrame>(text).ok()?.event {
            chat::ServerEvent::Restarting { retry_after_ms } => Some(retry_after_ms),
            _ => None,
        }
    })
}

async fn chat_socket(addr: std::net::SocketAddr, token: &str, notice: bool) -> Socket {
    let query = if notice {
        format!("?{}", chat::RESTART_NOTICE_QUERY)
    } else {
        String::new()
    };
    let mut request = format!("ws://{addr}{}{query}", chat::SOCKET_PATH)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let frame = chat::ClientFrame {
        version: chat::SOCKET_VERSION,
        event: chat::ClientEvent::Connect {
            token: token.into(),
            guild_updates: true,
            permission_set: None,
        },
    };
    socket
        .send(Message::Text(serde_json::to_string(&frame).unwrap().into()))
        .await
        .unwrap();
    let authenticated = tokio::time::timeout(Duration::from_secs(10), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(matches!(
        serde_json::from_str::<chat::ServerFrame>(authenticated.to_text().unwrap())
            .unwrap()
            .event,
        chat::ServerEvent::Authenticated {}
    ));
    socket
}

fn rejoin(call: &str) -> Option<voice::JoinAttempt> {
    Some(voice::JoinAttempt {
        call_id: call.parse::<RequestId>().unwrap(),
        reconnect: true,
    })
}

impl Peer {
    /// Requests and completes an ICE restart; returns the fresh relay servers.
    async fn restart_ice(&mut self) -> Vec<voice::IceServer> {
        send(&mut self.socket, ClientEvent::IceRestart {}).await;
        let (sdp, ice_servers) = loop {
            match event(&mut self.socket).await {
                ServerEvent::IceRestart { sdp, ice_servers } => break (sdp, ice_servers),
                ServerEvent::Participants { .. }
                | ServerEvent::MediaDiagnostics { .. }
                | ServerEvent::ScreenTarget { .. } => {}
                _ => panic!("Expected an ICE restart offer"),
            }
        };
        let offer: RTCSessionDescription = serde_json::from_str(&sdp).unwrap();
        assert_eq!(offer.sdp_type, RTCSdpType::Offer);
        self.pc.set_remote_description(offer).await.unwrap();
        let answer = self.pc.create_answer(None).await.unwrap();
        self.pc.set_local_description(answer).await.unwrap();
        // Local candidates are kept, so the answer may already be complete.
        let _ = tokio::time::timeout(Duration::from_secs(2), self.gathered.notified()).await;
        send(
            &mut self.socket,
            ClientEvent::IceRestartAnswer {
                sdp: serde_json::to_string(&self.pc.local_description().await.unwrap()).unwrap(),
            },
        )
        .await;
        ice_servers
    }
}

fn ice_ufrag(sdp: &str) -> String {
    sdp.lines()
        .find_map(|line| line.trim_end().strip_prefix("a=ice-ufrag:"))
        .unwrap()
        .to_owned()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn planned_restart_notifies_closes_and_refuses_new_admission() {
    let f = fixture("drain").await;
    let shutdown = Shutdown::default();
    let app = api::router_with_shutdown(
        Some(f.db.pool.clone()),
        origins(),
        Limits::default(),
        shutdown.clone(),
    );
    let (addr, serving) = listen(app.clone()).await;
    let mut current =
        Peer::resilient(join(addr, &f.owner, f.guild.clone(), f.channel.clone()).await).await;
    let mut legacy =
        Peer::new(join(addr, &f.member, f.guild.clone(), f.channel.clone()).await).await;
    forwarded(&mut current, &mut legacy).await;
    let mut chat = chat_socket(addr, &f.owner, true).await;
    let mut legacy_chat = chat_socket(addr, &f.member, false).await;

    assert!(shutdown.begin());
    // A current client hears the restart, with a jittered first-retry hint.
    let (texts, code) = until_close(&mut current.socket).await;
    let hint = voice_notice(&texts).expect("negotiated sessions are told about the restart");
    assert!((RETRY_BASE_MS..=RETRY_BASE_MS + RETRY_JITTER_MS).contains(&hint));
    assert_eq!(code, Some(voice::RESTART_CLOSE_CODE));
    // An older client never receives the unknown event, only the 1012 close.
    let (texts, code) = until_close(&mut legacy.socket).await;
    assert_eq!(voice_notice(&texts), None);
    assert_eq!(code, Some(voice::RESTART_CLOSE_CODE));
    let (texts, code) = until_close(&mut chat).await;
    assert!(chat_notice(&texts).is_some_and(|hint| hint >= RETRY_BASE_MS));
    assert_eq!(code, Some(voice::RESTART_CLOSE_CODE));
    let (texts, code) = until_close(&mut legacy_chat).await;
    assert_eq!(chat_notice(&texts), None, "chat notices are opt-in");
    assert_eq!(code, Some(voice::RESTART_CLOSE_CODE));

    // Admission is closed while draining; liveness stays independent.
    let status = |path: &'static str| {
        let app = app.clone();
        async move {
            app.oneshot(Request::builder().uri(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(
        status(thiscord_shared::READY_PATH).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert_eq!(status(thiscord_shared::HEALTH_PATH).await, StatusCode::OK);
    let mut request = format!("ws://{addr}{}", voice::VOICE_PATH)
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    match tokio_tungstenite::connect_async(request).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => {
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE)
        }
        _ => panic!("a draining server must refuse new voice sockets with a retryable 503"),
    }
    // Every room drained well inside the bound.
    assert_eq!(shutdown.wait_idle(Duration::from_secs(5)).await, 0);
    current.pc.close().await.unwrap();
    legacy.pc.close().await.unwrap();
    serving.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn rejoin_after_restart_passes_fresh_admission_and_permission_checks() {
    let mut f = fixture("rejoin").await;
    // The process being restarted.
    let first = Shutdown::default();
    let (first_addr, first_serving) = listen(api::router_with_shutdown(
        Some(f.db.pool.clone()),
        origins(),
        Limits::default(),
        first.clone(),
    ))
    .await;
    let owner_call = "00000000-0000-4000-8000-0000000000a1";
    let member_call = "00000000-0000-4000-8000-0000000000a2";
    let owner = Peer::resilient(
        join_attempt(
            first_addr,
            &f.owner,
            f.guild.clone(),
            f.channel.clone(),
            rejoin(owner_call).map(|attempt| voice::JoinAttempt {
                reconnect: false,
                ..attempt
            }),
        )
        .await,
    )
    .await;
    let member = Peer::resilient(
        join_attempt(
            first_addr,
            &f.member,
            f.guild.clone(),
            f.channel.clone(),
            rejoin(member_call).map(|attempt| voice::JoinAttempt {
                reconnect: false,
                ..attempt
            }),
        )
        .await,
    )
    .await;
    let member_secret = member.resume_token.clone();
    first.begin();
    assert_eq!(first.wait_idle(Duration::from_secs(5)).await, 0);
    first_serving.abort();
    owner.pc.close().await.unwrap();
    member.pc.close().await.unwrap();

    // The next process: same database, none of the previous in-memory state.
    let next = Shutdown::with_rejoin_window(Duration::from_secs(60));
    let app = api::router_with_shutdown(
        Some(f.db.pool.clone()),
        origins(),
        Limits::default(),
        next.clone(),
    );
    let (addr, serving) = listen(app.clone()).await;
    // A resume racing the restart is told to rejoin, not that the call ended.
    let mut resumed = open_voice(addr).await;
    resume(
        &mut resumed,
        &f.member,
        &f.guild,
        &f.channel,
        &member_secret,
        1,
    )
    .await;
    assert!(matches!(
        event(&mut resumed).await,
        ServerEvent::Restarting { retry_after_ms } if retry_after_ms <= 1_000
    ));
    // Outside the window an unknown session ends the call as before.
    let (settled_addr, settled_serving) =
        listen(api::router(Some(f.db.pool.clone()), origins())).await;
    let mut settled = open_voice(settled_addr).await;
    resume(
        &mut settled,
        &f.member,
        &f.guild,
        &f.channel,
        &member_secret,
        1,
    )
    .await;
    assert!(matches!(
        event(&mut settled).await,
        ServerEvent::ResumeUnavailable {}
    ));
    settled_serving.abort();

    // Access changed during the restart: the automatic rejoin is refused. The
    // pre-restart session and its grants are never trusted.
    change(
        &app,
        &f.owner,
        &mut f.state,
        json!({"action":"set_override","channel_id":f.channel,
            "target":{"kind":"member","id":f.member_id},"allow":[],"deny":["join_voice"]}),
        StatusCode::OK,
    )
    .await;
    let mut denied = join_attempt(
        addr,
        &f.member,
        f.guild.clone(),
        f.channel.clone(),
        rejoin(member_call),
    )
    .await;
    match event(&mut denied).await {
        ServerEvent::Error { error } => assert_eq!(error.code, ErrorCode::Forbidden),
        _ => panic!("a rejoin without current voice access must be refused"),
    }
    // A still-authorized participant rejoins the same call after full admission.
    let mut back = Peer::resilient(
        join_attempt(
            addr,
            &f.owner,
            f.guild.clone(),
            f.channel.clone(),
            rejoin(owner_call),
        )
        .await,
    )
    .await;
    still_connected(&mut back.socket).await;
    back.close().await;
    serving.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn ice_restart_keeps_the_session_and_media_flowing_both_ways() {
    let f = fixture("ice").await;
    let (addr, serving) = listen(api::router(Some(f.db.pool.clone()), origins())).await;
    let mut current =
        Peer::resilient(join(addr, &f.owner, f.guild.clone(), f.channel.clone()).await).await;
    let mut other =
        Peer::new(join(addr, &f.member, f.guild.clone(), f.channel.clone()).await).await;
    forwarded(&mut current, &mut other).await;
    let before = ice_ufrag(&current.pc.remote_description().await.unwrap().sdp);
    let slot = current.slot;
    current.restart_ice().await;
    let after = ice_ufrag(&current.pc.remote_description().await.unwrap().sdp);
    assert_ne!(
        before, after,
        "an ICE restart uses fresh server credentials"
    );
    // The same session (slot, tracks, DTLS) carries media in both directions.
    forwarded(&mut current, &mut other).await;
    forwarded(&mut other, &mut current).await;
    still_connected(&mut current.socket).await;
    assert_eq!(current.slot, slot);
    // Repeated restarts work too (e.g. a credential refresh after a path change).
    tokio::time::sleep(Duration::from_millis(2_100)).await;
    current.restart_ice().await;
    forwarded(&mut other, &mut current).await;
    forwarded(&mut current, &mut other).await;
    // A client that did not negotiate ICE restarts cannot request one.
    send(&mut other.socket, ClientEvent::IceRestart {}).await;
    let (_, code) = until_close(&mut other.socket).await;
    assert_ne!(code, Some(voice::RESTART_CLOSE_CODE));
    // An answer without an offer is a protocol error, never applied.
    send(
        &mut current.socket,
        ClientEvent::IceRestartAnswer { sdp: "{}".into() },
    )
    .await;
    until_close(&mut current.socket).await;
    current.pc.close().await.unwrap();
    other.pc.close().await.unwrap();
    serving.abort();
}
