use super::*;
use std::time::Duration;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn download_bodies_hold_admission_until_completed_or_dropped() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, _, _, state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let uri = upload_url(&guild, &channel, "file.bin", "text/plain");
    let attachment = upload_body(
        &app,
        &owner,
        &uri,
        Body::from(vec![0; 128 * 1024]),
        StatusCode::OK,
    )
    .await;
    chat(
        &app,
        &owner,
        send(&guild, &channel, json!([attachment["id"]])),
        StatusCode::OK,
    )
    .await;
    let mut held = Vec::new();
    for _ in 0..4 {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "{ATTACHMENTS_PATH}/{}",
                        attachment["id"].as_str().unwrap()
                    ))
                    .header("authorization", format!("Bearer {owner}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        held.push(response);
    }
    download(
        &app,
        &owner,
        &attachment["id"],
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    drop(held.pop());
    assert_eq!(
        download(&app, &owner, &attachment["id"], StatusCode::OK)
            .await
            .len(),
        128 * 1024
    );
    // Completion also frees its slot while the other three bodies remain unconsumed.
    assert_eq!(
        download(&app, &owner, &attachment["id"], StatusCode::OK)
            .await
            .len(),
        128 * 1024
    );
}

fn upload_url(guild: &Value, channel: &Value, filename: &str, mime: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("guild_id", guild.as_str().unwrap())
        .append_pair("channel_id", channel.as_str().unwrap())
        .append_pair("filename", filename)
        .append_pair("content_type", mime)
        .finish();
    format!("{ATTACHMENTS_PATH}?{query}")
}
async fn upload_body(
    app: &Router,
    token: &str,
    uri: &str,
    body: Body,
    expected: StatusCode,
) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), expected);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    if !expected.is_success() {
        assert_eq!(body["request_id"], id);
    }
    body
}
async fn download(app: &Router, token: &str, id: &Value, expected: StatusCode) -> Vec<u8> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{ATTACHMENTS_PATH}/{}", id.as_str().unwrap()))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), expected);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    if expected.is_success() {
        assert!(
            response.headers()["content-disposition"]
                .to_str()
                .unwrap()
                .starts_with("attachment;")
        );
        assert_eq!(
            response.headers()["content-type"],
            "application/octet-stream"
        );
    }
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let bytes = to_bytes(response.into_body(), MAX_ATTACHMENT_BYTES + 4096)
        .await
        .unwrap()
        .to_vec();
    if !expected.is_success() {
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["request_id"],
            request_id
        );
    }
    bytes
}
fn send(guild: &Value, channel: &Value, ids: Value) -> Value {
    json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"","attachment_ids":ids})
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn attachments_bind_owner_channel_replay_and_delete_atomically() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let payload = b"<svg onload='alert(1)'>private file</svg>";
    let uri = upload_url(&guild, &channel, "private.svg", "image/svg+xml");
    let attachment = upload_body(
        &app,
        &owner,
        &uri,
        Body::from(payload.as_slice()),
        StatusCode::OK,
    )
    .await;
    assert_eq!(attachment["content_type"], "application/octet-stream");
    assert_eq!(attachment["size_bytes"], payload.len());
    download(&app, &owner, &attachment["id"], StatusCode::FORBIDDEN).await;
    chat(
        &app,
        &guest,
        send(&guild, &channel, json!([attachment["id"]])),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"other","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let other = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] != channel)
        .unwrap()["id"]
        .clone();
    chat(
        &app,
        &owner,
        send(&guild, &other, json!([attachment["id"]])),
        StatusCode::FORBIDDEN,
    )
    .await;
    chat(
        &app,
        &owner,
        send(
            &guild,
            &channel,
            json!([attachment["id"], attachment["id"]]),
        ),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let request = send(&guild, &channel, json!([attachment["id"]]));
    let (first, retry) = tokio::join!(
        chat(&app, &owner, request.clone(), StatusCode::OK),
        chat(&app, &owner, request.clone(), StatusCode::OK)
    );
    assert_eq!(first, retry);
    assert_eq!(first["message"]["attachments"], json!([attachment]));
    let id = first["message"]["id"].clone();
    assert_eq!(
        download(&app, &guest, &attachment["id"], StatusCode::OK).await,
        payload
    );
    chat(
        &app,
        &owner,
        send(&guild, &channel, json!([attachment["id"]])),
        StatusCode::FORBIDDEN,
    )
    .await;
    let mut mismatch = request.clone();
    mismatch["attachment_ids"] = json!([Uuid::new_v4()]);
    chat(&app, &owner, mismatch, StatusCode::CONFLICT).await;
    let mut collision = request.clone();
    collision["content"] = serde_json::to_value(
        serde_json::to_string(&("", vec![attachment["id"].as_str().unwrap()])).unwrap(),
    )
    .unwrap();
    collision["attachment_ids"] = json!([]);
    chat(&app, &owner, collision, StatusCode::CONFLICT).await;
    let edit = json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":id,"revision":0,"content":"caption"});
    let edited = chat(&app, &owner, edit, StatusCode::OK).await;
    assert_eq!(
        edited["message"]["attachments"],
        first["message"]["attachments"]
    );
    assert_eq!(
        chat(&app, &owner, request.clone(), StatusCode::OK).await,
        edited
    );
    let history = chat(
        &app,
        &guest,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        history["history"]["messages"][0]["attachments"],
        json!([attachment])
    );
    let removed = chat(&app, &owner, json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":id,"revision":1}), StatusCode::OK).await;
    assert_eq!(removed["message"]["attachments"], json!([]));
    assert_eq!(chat(&app, &owner, request, StatusCode::OK).await, removed);
    download(&app, &owner, &attachment["id"], StatusCode::FORBIDDEN).await;
    let count: i64 = crate_count(&db, "SELECT count(*)::bigint AS count FROM attachments");
    assert_eq!(count, 0);
}

fn crate_count(db: &Database, sql: &str) -> i64 {
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        count: i64,
    }
    diesel::sql_query(sql)
        .get_result::<Count>(&mut db.pool.get().unwrap())
        .unwrap()
        .count
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn attachments_enforce_live_access_and_revoke_downloads() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let uri = upload_url(&guild, &channel, "document.html", "text/html");
    let attachment = upload_body(
        &app,
        &owner,
        &uri,
        Body::from("<html>private</html>"),
        StatusCode::OK,
    )
    .await;
    chat(
        &app,
        &owner,
        send(&guild, &channel, json!([attachment["id"]])),
        StatusCode::OK,
    )
    .await;
    let (_, stranger) = user(&db, "outsider");
    download(&app, &stranger, &attachment["id"], StatusCode::FORBIDDEN).await;
    upload_body(
        &app,
        &stranger,
        &uri,
        Body::from("x"),
        StatusCode::FORBIDDEN,
    )
    .await;
    download(&app, "invalid", &attachment["id"], StatusCode::UNAUTHORIZED).await;
    for permission in ["read_history", "view_channel"] {
        change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":[permission]}),StatusCode::OK).await;
        download(&app, &guest, &attachment["id"], StatusCode::FORBIDDEN).await;
    }
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":[]}),StatusCode::OK).await;
    download(&app, &guest, &attachment["id"], StatusCode::OK).await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member","account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    download(&app, &guest, &attachment["id"], StatusCode::FORBIDDEN).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn attachments_enforce_size_count_expiration_and_quota() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, _, _, state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let uri = upload_url(&guild, &channel, "file.bin", "text/html");
    upload_body(&app, &owner, &uri, Body::empty(), StatusCode::BAD_REQUEST).await;
    upload_body(
        &app,
        &owner,
        &uri,
        Body::from(vec![0; MAX_ATTACHMENT_BYTES + 1]),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let five: Vec<_> = (0..5).map(|_| Uuid::new_v4()).collect();
    chat(
        &app,
        &owner,
        send(&guild, &channel, json!(five)),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let mut ids = Vec::new();
    for _ in 0..MAX_STAGED_ATTACHMENTS {
        ids.push(
            upload_body(&app, &owner, &uri, Body::from("x"), StatusCode::OK).await["id"].clone(),
        );
    }
    upload_body(
        &app,
        &owner,
        &uri,
        Body::from("x"),
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    diesel::sql_query("UPDATE attachments SET expires_at=now()-interval '1 second'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    chat(
        &app,
        &owner,
        send(&guild, &channel, json!([ids[0]])),
        StatusCode::FORBIDDEN,
    )
    .await;
    // The next upload reaps expired bytes without requiring a running background task.
    let big = upload_body(
        &app,
        &owner,
        &uri,
        Body::from(vec![0; MAX_ATTACHMENT_BYTES]),
        StatusCode::OK,
    )
    .await;
    assert_eq!(big["size_bytes"], MAX_ATTACHMENT_BYTES);
    assert_eq!(
        crate_count(&db, "SELECT count(*)::bigint AS count FROM attachments"),
        1
    );
    for _ in 1..4 {
        upload_body(
            &app,
            &owner,
            &uri,
            Body::from(vec![0; MAX_ATTACHMENT_BYTES]),
            StatusCode::OK,
        )
        .await;
    }
    upload_body(
        &app,
        &owner,
        &uri,
        Body::from("x"),
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn attachment_admission_and_auth_precede_buffering_and_recheck_after_body() {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let uri = upload_url(&guild, &channel, "file", "text/plain");
    let polled = Arc::new(AtomicBool::new(false));
    let signal = polled.clone();
    let unread_body = Body::from_stream(futures_util::stream::poll_fn(move |_| {
        signal.store(true, Ordering::SeqCst);
        std::task::Poll::<Option<Result<Vec<u8>, std::io::Error>>>::Pending
    }));
    upload_body(&app, "invalid", &uri, unread_body, StatusCode::UNAUTHORIZED).await;
    assert!(!polled.load(Ordering::SeqCst));
    let mut tasks = Vec::new();
    let mut releases = Vec::new();
    for _ in 0..4 {
        let (entered, entered_rx) = tokio::sync::oneshot::channel();
        let (release, release_rx) = tokio::sync::oneshot::channel();
        let body = Body::from_stream(futures_util::stream::once(async move {
            entered.send(()).unwrap();
            release_rx.await.unwrap();
            Ok::<_, std::io::Error>(vec![1u8])
        }));
        let (app, guest, uri) = (app.clone(), guest.clone(), uri.clone());
        tasks.push(tokio::spawn(async move {
            upload_body(&app, &guest, &uri, body, StatusCode::FORBIDDEN).await
        }));
        tokio::time::timeout(Duration::from_secs(3), entered_rx)
            .await
            .unwrap()
            .unwrap();
        releases.push(release);
    }
    let blocked = Body::from_stream(futures_util::stream::pending::<
        Result<Vec<u8>, std::io::Error>,
    >());
    tokio::time::timeout(
        Duration::from_secs(1),
        upload_body(&app, &owner, &uri, blocked, StatusCode::TOO_MANY_REQUESTS),
    )
    .await
    .unwrap();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member","account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    for release in releases {
        release.send(()).unwrap();
    }
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(
        crate_count(&db, "SELECT count(*)::bigint AS count FROM attachments"),
        0
    );
    let broken = Body::from_stream(futures_util::stream::once(async {
        Err::<Vec<u8>, _>(std::io::Error::other("test body failure"))
    }));
    upload_body(&app, &owner, &uri, broken, StatusCode::BAD_REQUEST).await;
}
