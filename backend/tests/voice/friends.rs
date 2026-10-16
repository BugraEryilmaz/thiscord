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
