use super::*;

/// The next event that is not routine call traffic.
async fn final_event(socket: &mut Socket) -> ServerEvent {
    loop {
        match event(socket).await {
            ServerEvent::Participants { .. }
            | ServerEvent::MediaDiagnostics { .. }
            | ServerEvent::ScreenTarget { .. }
            | ServerEvent::Pong {} => {}
            other => return other,
        }
    }
}

/// A microphone delivering audio faster than real time (seen with a stuck
/// Windows capture driver) exceeds the per-track ingress limit. The server must
/// name that cause in a final error, not revoke access or close silently, and a
/// fresh join with a healthy sender must work again.
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn excess_microphone_rate_is_refused_with_a_final_reason() {
    let db = database();
    let (_, ot) = user(&db, "refusal_owner");
    let (_, mt) = user(&db, "refusal_member");
    permissions::bootstrap_owner(&db.pool, "refusal_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Refused media"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"refusal_member"}),
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
    let mut flooding = Peer::new(join(addr, &ot, guild.clone(), channel.clone()).await).await;
    let mut listening = Peer::new(join(addr, &mt, guild.clone(), channel.clone()).await).await;
    // Media flows first, so the refusal below is about rate, not setup.
    forwarded(&mut flooding, &mut listening).await;
    // About 4.6x real time, as the failing driver delivered: far above the
    // limit of 100 packets within one rate window.
    for _ in 0..230 {
        flooding.publish().await;
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    match tokio::time::timeout(Duration::from_secs(5), final_event(&mut flooding.socket))
        .await
        .expect("refused media must be reported promptly")
    {
        ServerEvent::Error { error } => {
            assert_eq!(error.code, thiscord_shared::ErrorCode::BadRequest);
            assert!(error.message.contains("microphone sent audio faster"));
        }
        _ => panic!("expected a refusal error"),
    }
    // The refusal is final: the server closes instead of awaiting a resume.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match flooding.socket.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "refused connection must close");
    still_connected(&mut listening.socket).await;
    flooding.close().await;

    // No penalty survives the connection: a healthy sender rejoins at once.
    let mut rejoined = Peer::new(join(addr, &ot, guild, channel).await).await;
    forwarded(&mut rejoined, &mut listening).await;
    still_connected(&mut rejoined.socket).await;
    rejoined.close().await;
    listening.close().await;
    serving.abort();
}
