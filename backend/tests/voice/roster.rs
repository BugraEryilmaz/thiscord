use super::*;

async fn observer(address: std::net::SocketAddr, token: &str, guild: &Value) -> Socket {
    let mut req = format!("ws://{address}/api/v1/socket")
        .into_client_request()
        .unwrap();
    req.headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = tokio_tungstenite::connect_async(req).await.unwrap();
    socket
        .send(Message::Text(
            json!({"version":1,"event":{"type":"connect","token":token}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    socket.send(Message::Text(json!({"version":1,"event":{"type":"subscribe","subscription":1,"guild_id":guild,"channel_id":null,"voice_roster":true}}).to_string().into())).await.unwrap();
    socket
}

async fn roster(socket: &mut Socket) -> Value {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_ne!(frame["event"]["type"], "revoked");
            assert_ne!(frame["event"]["type"], "error");
            if frame["event"]["type"] == "update"
                && frame["event"]["event"]["type"] == "voice_roster"
            {
                assert_eq!(frame["event"]["subscription"], 1);
                return frame["event"]["event"]["channels"].clone();
            }
        }
    })
    .await
    .expect("voice roster did not update")
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn visible_rosters_without_join_permission_follow_membership_and_revoke() {
    let db = database();
    let (owner, ot) = user(&db, "roster_owner");
    let (viewer, vt) = user(&db, "roster_viewer");
    let (_, outsider) = user(&db, "roster_outsider");
    permissions::bootstrap_owner(&db.pool, "roster_owner").unwrap();
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Roster test"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"roster_viewer"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"visible","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"hidden","kind":"voice"}),
        StatusCode::OK,
    )
    .await;
    let hidden = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "hidden")
        .unwrap()["id"]
        .clone();
    for (id, denied) in [(&channel, "join_voice"), (&hidden, "view_channel")] {
        change(&app, &ot, &mut state, json!({"action":"set_override","channel_id":id,"target":{"kind":"member","id":viewer},"allow":[],"deny":[denied]}), StatusCode::OK).await;
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let serving = tokio::spawn(axum::serve(listener, app.clone()).into_future());
    let mut watching = observer(address, &vt, &guild).await;
    assert_eq!(
        roster(&mut watching).await,
        json!([{"channel_id":channel,"members":[]}])
    );

    let mut peer = Peer::new(join(address, &ot, guild.clone(), channel.clone()).await).await;
    let joined = roster(&mut watching).await;
    assert_eq!(joined.as_array().unwrap().len(), 1);
    assert_eq!(joined[0]["members"][0]["account_id"], owner.to_string());
    assert_eq!(joined[0]["members"][0]["display_name"], "roster_owner");
    send(
        &mut peer.socket,
        ClientEvent::State {
            muted: true,
            deafened: true,
        },
    )
    .await;
    let muted = roster(&mut watching).await;
    assert_eq!(muted[0]["members"][0]["muted"], true);
    assert_eq!(muted[0]["members"][0]["deafened"], true);
    peer.close().await;
    assert_eq!(
        roster(&mut watching).await,
        json!([{"channel_id":channel,"members":[]}])
    );

    // No implicit guild access, even for authenticated accounts.
    let mut denied = observer(address, &outsider, &guild).await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let Message::Text(text) = denied.next().await.unwrap().unwrap() else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_ne!(frame["event"]["event"]["type"], "voice_roster");
            if frame["event"]["event"]["type"] == "error" {
                break;
            }
        }
    })
    .await
    .unwrap();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"remove_member","account_id":viewer}),
        StatusCode::OK,
    )
    .await;
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let Message::Text(text) = watching.next().await.unwrap().unwrap() else {
                continue;
            };
            let frame: Value = serde_json::from_str(&text).unwrap();
            assert_ne!(frame["event"]["event"]["type"], "voice_roster");
            if frame["event"]["type"] == "revoked" {
                break;
            }
        }
    })
    .await
    .unwrap();
    let _ = watching.close(None).await;
    let _ = denied.close(None).await;
    serving.abort();
}
