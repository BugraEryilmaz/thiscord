use super::*;

async fn hinted_socket(address: std::net::SocketAddr, token: &str) -> Socket {
    let mut request = format!("ws://{address}{SOCKET_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"connect","token":token,"guild_updates":true}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    event(&mut socket, "authenticated").await;
    socket
}
/// Pings and returns every event type received before the pong.
async fn drained(socket: &mut Socket) -> Vec<String> {
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    socket
        .send(WsMessage::Text(
            json!({"version":1,"event":{"type":"ping"}})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let mut seen = Vec::new();
    tokio::time::timeout(std::time::Duration::from_secs(6), async {
        loop {
            if let WsMessage::Text(text) = socket.next().await.unwrap().unwrap() {
                let value: Value = serde_json::from_str(&text).unwrap();
                let kind = value["event"]["type"].as_str().unwrap().to_owned();
                if kind == "pong" {
                    break;
                }
                seen.push(kind);
            }
        }
    })
    .await
    .expect("socket event timeout");
    seen
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn guild_presentation_hints_reach_only_opted_in_members() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, mut state, _) = setup(&db).await;
    let (_, outsider) = user(&db, "hint_outsider");
    let guild = state["guild"]["id"].clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut member = hinted_socket(address, &guest).await;
    let mut stranger = hinted_socket(address, &outsider).await;
    let mut legacy = session_socket(address, &guest).await;
    event(&mut legacy, "authenticated").await;

    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"rename","name":"Renamed"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(event(&mut member, "guild_updated").await["guild_id"], guild);

    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(8, 8)
        .write_to(&mut png, image::ImageFormat::Png)
        .unwrap();
    let image = base64::engine::general_purpose::STANDARD.encode(png.into_inner());
    let (status, body) = call(
        &app,
        GUILD_ICON_PATH,
        &owner,
        json!({"guild_id":guild,"revision":state["guild"]["revision"],"image_base64":image}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(event(&mut member, "guild_updated").await["guild_id"], guild);
    // A rejected change publishes nothing.
    let (status, _) = call(
        &app,
        GUILD_ICON_PATH,
        &guest,
        json!({"guild_id":guild,"revision":body["revision"],"image_base64":null}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!drained(&mut member).await.contains(&"guild_updated".into()));
    // Non-members and sockets that did not opt in never receive the hint.
    assert!(
        !drained(&mut stranger)
            .await
            .contains(&"guild_updated".into())
    );
    assert!(!drained(&mut legacy).await.contains(&"guild_updated".into()));
    server.abort();
}
