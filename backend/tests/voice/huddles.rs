use super::*;
use thiscord_shared::huddles::HUDDLES_PATH;

async fn blocked(source: &mut Peer, receiver: &mut Peer) {
    // Drain packets sent before the split; newly published packets must stop.
    tokio::time::sleep(Duration::from_millis(100)).await;
    while receiver.packets.try_recv().is_ok() {}
    for _ in 0..10 {
        source.publish().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(25), receiver.packets.recv())
                .await
                .is_err(),
            "a split room must not forward voices between huddles"
        );
    }
}

async fn huddles(app: &Router, token: &str, request: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, HUDDLES_PATH, token, request).await;
    assert_eq!(status, expected, "{body}");
    body
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn huddles_need_permission_and_split_voices_without_reconnecting() {
    let db = database();
    let (_, ot) = user(&db, "huddle_owner");
    let (planner, pt) = user(&db, "huddle_planner");
    let (player, lt) = user(&db, "huddle_player");
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Huddles"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    for name in ["huddle_planner", "huddle_player"] {
        change(
            &app,
            &ot,
            &mut state,
            json!({"action":"add_member","username":name}),
            StatusCode::OK,
        )
        .await;
    }
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"duo","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "duo")
        .unwrap()["id"]
        .clone();
    let request = |change: Value| json!({"guild_id":guild,"channel_id":channel,"change":change});
    let split = |active: bool| request(json!({"action":"split","active":active}));
    // Huddles belong to a live room.
    huddles(&app, &ot, split(true), StatusCode::BAD_REQUEST).await;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut owner = Peer::new(join(addr, &ot, guild.clone(), channel.clone()).await).await;
    let mut planning = Peer::new(join(addr, &pt, guild.clone(), channel.clone()).await).await;
    let mut playing = Peer::new(join(addr, &lt, guild.clone(), channel.clone()).await).await;
    forwarded(&mut playing, &mut owner).await;

    // Being in the room is not enough; Everyone has no ManageHuddles.
    let assign = |account: AccountId, huddle: u8| {
        request(json!({"action":"assign","account_id":account,"huddle":huddle}))
    };
    huddles(&app, &lt, assign(player, 1), StatusCode::FORBIDDEN).await;
    huddles(&app, &pt, split(true), StatusCode::FORBIDDEN).await;
    change(&app, &ot, &mut state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":planner},"allow":["manage_huddles"],"deny":[]}), StatusCode::OK).await;
    let planned = huddles(&app, &pt, assign(player, 1), StatusCode::OK).await;
    assert_eq!(
        planned,
        json!({"split":false,"assignments":[{"account_id":player,"huddle":1}]})
    );
    // Bounded numbers and people currently in the room only.
    huddles(&app, &pt, assign(player, 5), StatusCode::BAD_REQUEST).await;
    let stranger = AccountId::from_uuid(uuid::Uuid::new_v4());
    huddles(&app, &pt, assign(stranger, 2), StatusCode::BAD_REQUEST).await;
    // Planning alone keeps everyone together.
    forwarded(&mut playing, &mut owner).await;

    let state_after = huddles(&app, &pt, split(true), StatusCode::OK).await;
    assert_eq!(state_after["split"], true);
    // Same transports, no reconnect: huddle 1 is cut off from the main room
    // in both directions, while the main room still hears itself.
    blocked(&mut playing, &mut owner).await;
    blocked(&mut owner, &mut playing).await;
    blocked(&mut planning, &mut playing).await;
    forwarded(&mut owner, &mut planning).await;
    still_connected(&mut playing.socket).await;
    // Splitting twice (two people pressing the shortcut) converges.
    huddles(&app, &ot, split(true), StatusCode::OK).await;
    blocked(&mut playing, &mut owner).await;

    // Losing the permission stops further changes.
    change(&app, &ot, &mut state, json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":planner},"allow":[],"deny":["manage_huddles"]}), StatusCode::OK).await;
    huddles(&app, &pt, split(false), StatusCode::FORBIDDEN).await;
    huddles(&app, &ot, split(false), StatusCode::OK).await;
    forwarded(&mut playing, &mut owner).await;
    forwarded(&mut owner, &mut playing).await;

    // Clearing forgets the plan.
    let cleared = huddles(
        &app,
        &ot,
        request(json!({"action":"clear"})),
        StatusCode::OK,
    )
    .await;
    assert_eq!(cleared, json!({"split":false,"assignments":[]}));
    for peer in [owner, planning, playing] {
        peer.close().await;
    }
    serving.abort();
}
