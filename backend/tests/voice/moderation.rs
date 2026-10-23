use super::*;

async fn flags(
    socket: &mut Socket,
    account: AccountId,
    muted: bool,
    deafened: bool,
) -> voice::Participant {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match event(socket).await {
                ServerEvent::Participants { members } => {
                    if let Some(m) = members.into_iter().find(|m| {
                        m.account_id == account
                            && m.server_muted == muted
                            && m.server_deafened == deafened
                    }) {
                        return m;
                    }
                }
                ServerEvent::Revoked {} | ServerEvent::Error { .. } => {
                    panic!("moderation flag change must retain the connection")
                }
                _ => {}
            }
        }
    })
    .await
    .unwrap()
}
async fn blocked(source: &mut Peer, receiver: &mut Peer) {
    // Drain pre-commit network packets; newly published packets must be fenced.
    tokio::time::sleep(Duration::from_millis(100)).await;
    while receiver.packets.try_recv().is_ok() {}
    for _ in 0..10 {
        source.publish().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(25), receiver.packets.recv())
                .await
                .is_err()
        );
    }
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn moderator_flags_fence_both_media_directions_preserve_self_state_and_move() {
    let db = database();
    let (_, ot) = user(&db, "voice_mod_owner");
    let (member, mt) = user(&db, "voice_mod_member");
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Moderated voice"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"voice_mod_member"}),
        StatusCode::OK,
    )
    .await;
    for name in ["source", "destination"] {
        change(
            &app,
            &ot,
            &mut state,
            json!({"action":"create_channel","name":name,"kind":"voice"}),
            StatusCode::OK,
        )
        .await;
    }
    let source = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "source")
        .unwrap()["id"]
        .clone();
    let destination = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "destination")
        .unwrap()["id"]
        .clone();
    // A disconnected user cannot be silently started by a moderator.
    change(&app,&ot,&mut state,json!({"action":"move_voice","account_id":member,"source_channel_id":source,"channel_id":destination}),StatusCode::CONFLICT).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut a = Peer::new(join(addr, &ot, guild.clone(), source.clone()).await).await;
    let mut b = Peer::new(join(addr, &mt, guild.clone(), source.clone()).await).await;
    forwarded(&mut b, &mut a).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_mute","account_id":member,"muted":true}),
        StatusCode::OK,
    )
    .await;
    flags(&mut b.socket, member, true, false).await;
    send(
        &mut b.socket,
        ClientEvent::State {
            muted: false,
            deafened: false,
        },
    )
    .await;
    still_connected(&mut b.socket).await;
    blocked(&mut b, &mut a).await;
    forwarded(&mut a, &mut b).await;
    // Moderator release never clears the user's own mute or deafen.
    send(
        &mut b.socket,
        ClientEvent::State {
            muted: true,
            deafened: true,
        },
    )
    .await;
    still_connected(&mut b.socket).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_deafen","account_id":member,"deafened":true}),
        StatusCode::OK,
    )
    .await;
    flags(&mut b.socket, member, true, true).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_mute","account_id":member,"muted":false}),
        StatusCode::OK,
    )
    .await;
    flags(&mut b.socket, member, false, true).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_deafen","account_id":member,"deafened":false}),
        StatusCode::OK,
    )
    .await;
    let own = flags(&mut b.socket, member, false, false).await;
    assert!(own.muted && own.deafened);
    blocked(&mut b, &mut a).await;
    blocked(&mut a, &mut b).await;
    send(
        &mut b.socket,
        ClientEvent::State {
            muted: false,
            deafened: false,
        },
    )
    .await;
    still_connected(&mut b.socket).await;
    forwarded(&mut b, &mut a).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_deafen","account_id":member,"deafened":true}),
        StatusCode::OK,
    )
    .await;
    flags(&mut b.socket, member, false, true).await;
    blocked(&mut a, &mut b).await;
    blocked(&mut b, &mut a).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_deafen","account_id":member,"deafened":false}),
        StatusCode::OK,
    )
    .await;
    flags(&mut b.socket, member, false, false).await;
    // Destination denial applies to move, even when the actor is the owner.
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":destination,"target":{"kind":"member","id":member},"allow":[],"deny":["join_voice"]}),StatusCode::OK).await;
    change(&app,&ot,&mut state,json!({"action":"move_voice","account_id":member,"source_channel_id":source,"channel_id":destination}),StatusCode::FORBIDDEN).await;
    change(&app,&ot,&mut state,json!({"action":"delete_override","channel_id":destination,"target":{"kind":"member","id":member}}),StatusCode::OK).await;
    change(&app,&ot,&mut state,json!({"action":"move_voice","account_id":member,"source_channel_id":source,"channel_id":destination}),StatusCode::OK).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match event(&mut b.socket).await {
                ServerEvent::Moved { channel_id } => {
                    assert_eq!(json!(channel_id), destination);
                    break;
                }
                ServerEvent::Revoked {} => panic!("expected explicit move"),
                _ => {}
            }
        }
    })
    .await
    .unwrap();
    blocked(&mut b, &mut a).await;
    b.close().await;
    let mut moved = Peer::new(join(addr, &mt, guild.clone(), destination.clone()).await).await;
    still_connected(&mut moved.socket).await;
    blocked(&mut moved, &mut a).await;
    a.close().await;
    moved.close().await;
    serving.abort();
    let _ = serving.await;
}
