use super::*;
use thiscord_shared::friends::FRIENDS_PATH;

async fn social(app: &Router, token: &str, command: Value) -> Value {
    let (status, body) = call(app, FRIENDS_PATH, token, command).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}
async fn revoked(socket: &mut Socket) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match event(socket).await {
                ServerEvent::Revoked {} | ServerEvent::Error { .. } => break,
                _ => {}
            }
        }
    })
    .await
    .expect("private media must be revoked");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL and loopback WebRTC"]
async fn direct_calls_require_acceptance_and_revoke_both_media_peers() {
    let db = database();
    let (a_id, a) = user(&db, "call_alice");
    let (b_id, b) = user(&db, "call_bob");
    let (_, outsider) = user(&db, "call_outsider");
    let conversation = Uuid::new_v4();
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'accepted')").bind::<Text,_>(conversation.to_string()).bind::<Text,_>(a_id.to_string()).bind::<Text,_>(b_id.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            service.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    for remove in [false, true] {
        let started = social(
            &app,
            &a,
            json!({"action":"start_call","conversation_id":conversation}),
        )
        .await;
        let id = started["call"]["id"].clone();
        let mut premature = join(address, &a, id.clone(), id.clone()).await;
        assert!(matches!(
            event(&mut premature).await,
            ServerEvent::Error { .. }
        ));
        social(&app, &b, json!({"action":"accept_call","call_id":id})).await;
        let mut stranger = join(address, &outsider, id.clone(), id.clone()).await;
        assert!(matches!(
            event(&mut stranger).await,
            ServerEvent::Error { .. }
        ));
        let mut alice = Peer::new(join(address, &a, id.clone(), id.clone()).await).await;
        let mut bob = Peer::new(join(address, &b, id.clone(), id.clone()).await).await;
        forwarded(&mut alice, &mut bob).await;
        forwarded(&mut bob, &mut alice).await;
        camera_grid(&mut alice, &mut bob).await;
        if remove {
            social(&app, &a, json!({"action":"remove","account_id":b_id})).await;
        } else {
            social(&app, &b, json!({"action":"end_call","call_id":id})).await;
        }
        revoked(&mut alice.socket).await;
        revoked(&mut bob.socket).await;
        let mut late = join(address, &a, id.clone(), id).await;
        assert!(matches!(event(&mut late).await, ServerEvent::Error { .. }));
        alice.close().await;
        bob.close().await;
    }
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL and loopback WebRTC"]
async fn blocking_revokes_both_private_media_directions_and_unblock_cannot_rejoin() {
    let db = database();
    let (aid, a) = user(&db, "blocked_call_a");
    let (bid, b) = user(&db, "blocked_call_b");
    let conversation = Uuid::new_v4();
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'accepted')").bind::<Text,_>(conversation.to_string()).bind::<Text,_>(aid.to_string()).bind::<Text,_>(bid.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            service.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let id = social(
        &app,
        &a,
        json!({"action":"start_call","conversation_id":conversation}),
    )
    .await["call"]["id"]
        .clone();
    social(&app, &b, json!({"action":"accept_call","call_id":id})).await;
    let mut alice = Peer::new(join(address, &a, id.clone(), id.clone()).await).await;
    let mut bob = Peer::new(join(address, &b, id.clone(), id.clone()).await).await;
    forwarded(&mut alice, &mut bob).await;
    forwarded(&mut bob, &mut alice).await;
    assert_eq!(
        call(
            &app,
            thiscord_shared::safety::SAFETY_PATH,
            &b,
            json!({"action":"block","account_id":aid})
        )
        .await
        .0,
        StatusCode::OK
    );
    revoked(&mut alice.socket).await;
    revoked(&mut bob.socket).await;
    assert_eq!(
        call(
            &app,
            thiscord_shared::safety::SAFETY_PATH,
            &b,
            json!({"action":"unblock","account_id":aid})
        )
        .await
        .0,
        StatusCode::OK
    );
    let mut late = join(address, &a, id.clone(), id).await;
    assert!(matches!(event(&mut late).await, ServerEvent::Error { .. }));
    assert_eq!(
        call(
            &app,
            FRIENDS_PATH,
            &a,
            json!({"action":"start_call","conversation_id":conversation})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    alice.close().await;
    bob.close().await;
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL and loopback WebRTC"]
async fn group_calls_admit_joined_members_and_removal_revokes_only_that_peer() {
    let db = database();
    let (aid, a) = user(&db, "group_call_a");
    let (bid, b) = user(&db, "group_call_b");
    let (cid, c) = user(&db, "group_call_c");
    let (_, outsider) = user(&db, "group_call_out");
    for other in [bid, cid] {
        diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES(gen_random_uuid(),LEAST($1::uuid,$2::uuid),GREATEST($1::uuid,$2::uuid),$1::uuid,'accepted')").bind::<Text,_>(aid.to_string()).bind::<Text,_>(other.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    }
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(
            listener,
            service.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap()
    });
    let created = social(
        &app,
        &a,
        json!({"action":"create_group","client_id":Uuid::new_v4(),"members":[bid,cid]}),
    )
    .await;
    let group = created["group"]["conversation_id"].clone();
    let id = social(
        &app,
        &a,
        json!({"action":"start_call","conversation_id":group}),
    )
    .await["call"]["id"]
        .clone();
    // Ringing is not a joined lease, and outsiders are never admitted.
    for token in [&c, &outsider] {
        let mut early = join(address, token, id.clone(), id.clone()).await;
        assert!(matches!(event(&mut early).await, ServerEvent::Error { .. }));
    }
    social(&app, &b, json!({"action":"accept_call","call_id":id})).await;
    social(&app, &c, json!({"action":"accept_call","call_id":id})).await;
    let mut alice = Peer::new(join(address, &a, id.clone(), id.clone()).await).await;
    let mut bob = Peer::new(join(address, &b, id.clone(), id.clone()).await).await;
    let mut carol = Peer::new(join(address, &c, id.clone(), id.clone()).await).await;
    forwarded(&mut alice, &mut bob).await;
    forwarded(&mut bob, &mut carol).await;
    forwarded(&mut carol, &mut alice).await;
    let revision = created["group"]["revision"].clone();
    social(&app, &a, json!({"action":"remove_group_member","conversation_id":group,"revision":revision,"account_id":cid})).await;
    revoked(&mut carol.socket).await;
    let mut late = join(address, &c, id.clone(), id.clone()).await;
    assert!(matches!(event(&mut late).await, ServerEvent::Error { .. }));
    // The rest of the group keeps talking.
    forwarded(&mut alice, &mut bob).await;
    forwarded(&mut bob, &mut alice).await;
    // Leaving drains only the leaver; the last member out ends the call.
    social(&app, &b, json!({"action":"end_call","call_id":id})).await;
    revoked(&mut bob.socket).await;
    social(&app, &a, json!({"action":"end_call","call_id":id})).await;
    revoked(&mut alice.socket).await;
    let mut ended = join(address, &a, id.clone(), id).await;
    assert!(matches!(event(&mut ended).await, ServerEvent::Error { .. }));
    alice.close().await;
    bob.close().await;
    carol.close().await;
    server.abort();
}

#[derive(diesel::QueryableByName)]
struct Held {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    held: i64,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL and loopback WebRTC"]
async fn group_call_survives_a_restart_and_a_removed_member_cannot_rejoin() {
    use thiscord_backend::{limits::Limits, shutdown::Shutdown};
    let db = database();
    let (aid, a) = user(&db, "restart_group_a");
    let (bid, b) = user(&db, "restart_group_b");
    let (cid, c) = user(&db, "restart_group_c");
    for other in [bid, cid] {
        diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES(gen_random_uuid(),LEAST($1::uuid,$2::uuid),GREATEST($1::uuid,$2::uuid),$1::uuid,'accepted')").bind::<Text,_>(aid.to_string()).bind::<Text,_>(other.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    }
    let origins = || vec!["http://localhost:1420".parse().unwrap()];
    let listen = |app: Router| async move {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let _ = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .await;
        });
        (address, server)
    };
    // The process being restarted.
    let first = Shutdown::default();
    let app = api::router_with_shutdown(
        Some(db.pool.clone()),
        origins(),
        Limits::default(),
        first.clone(),
    );
    let (address, server) = listen(app.clone()).await;
    let created = social(
        &app,
        &a,
        json!({"action":"create_group","client_id":Uuid::new_v4(),"members":[bid,cid]}),
    )
    .await;
    let group = created["group"]["conversation_id"].clone();
    let revision = created["group"]["revision"].clone();
    let id = social(
        &app,
        &a,
        json!({"action":"start_call","conversation_id":group}),
    )
    .await["call"]["id"]
        .clone();
    social(&app, &b, json!({"action":"accept_call","call_id":id})).await;
    social(&app, &c, json!({"action":"accept_call","call_id":id})).await;
    let attempt = |n: u8, reconnect: bool| {
        Some(voice::JoinAttempt {
            call_id: format!("00000000-0000-4000-8000-0000000000c{n}")
                .parse()
                .unwrap(),
            reconnect,
        })
    };
    let mut alice =
        Peer::resilient(join_attempt(address, &a, id.clone(), id.clone(), attempt(1, false)).await)
            .await;
    let mut bob =
        Peer::resilient(join_attempt(address, &b, id.clone(), id.clone(), attempt(2, false)).await)
            .await;
    let carol =
        Peer::resilient(join_attempt(address, &c, id.clone(), id.clone(), attempt(3, false)).await)
            .await;
    forwarded(&mut alice, &mut bob).await;

    first.begin();
    assert_eq!(first.wait_idle(Duration::from_secs(5)).await, 0);
    server.abort();
    // Every drained member keeps a joined lease well past the ordinary 60 s.
    let held = diesel::sql_query("SELECT count(*) AS held FROM group_call_members WHERE call_id=$1::uuid AND state='joined' AND until>now()+interval '120 seconds'")
        .bind::<Text, _>(id.as_str().unwrap())
        .get_result::<Held>(&mut db.pool.get().unwrap())
        .unwrap()
        .held;
    assert_eq!(held, 3);
    for peer in [alice, bob, carol] {
        peer.pc.close().await.unwrap();
    }

    // The next process. The owner removes Carol while everyone is rejoining.
    let next = Shutdown::with_rejoin_window(Duration::from_secs(60));
    let app = api::router_with_shutdown(Some(db.pool.clone()), origins(), Limits::default(), next);
    let (address, server) = listen(app.clone()).await;
    social(&app, &a, json!({"action":"remove_group_member","conversation_id":group,"revision":revision,"account_id":cid})).await;
    // Remaining members rejoin the same group call after full admission.
    let mut alice =
        Peer::resilient(join_attempt(address, &a, id.clone(), id.clone(), attempt(1, true)).await)
            .await;
    let mut bob =
        Peer::resilient(join_attempt(address, &b, id.clone(), id.clone(), attempt(2, true)).await)
            .await;
    forwarded(&mut alice, &mut bob).await;
    forwarded(&mut bob, &mut alice).await;
    // A removed member's automatic rejoin is refused: membership is rechecked.
    let mut refused = join_attempt(address, &c, id.clone(), id.clone(), attempt(3, true)).await;
    match event(&mut refused).await {
        ServerEvent::Error { error } => {
            assert_eq!(error.code, thiscord_shared::ErrorCode::Forbidden)
        }
        _ => panic!("a removed member must not rejoin the group call"),
    }
    alice.close().await;
    bob.close().await;
    server.abort();
}
