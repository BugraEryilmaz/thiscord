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

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn image_sizes_reach_messages_on_upload_and_by_backfill() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let png = |width, height| {
        let mut out = std::io::Cursor::new(Vec::new());
        image::RgbImage::new(width, height)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        Body::from(out.into_inner())
    };
    let uri = |name: &str, mime: &str| upload_url(&guild, &channel, name, mime);
    // The header decides, not the client's hint.
    let wide = upload_body(
        &app,
        &owner,
        &uri("wide.bin", "text/plain"),
        png(160, 90),
        StatusCode::OK,
    )
    .await;
    assert_eq!(wide["image"], json!({"width":160,"height":90}));
    let file = upload_body(
        &app,
        &owner,
        &uri("notes.png", "image/png"),
        Body::from("plain"),
        StatusCode::OK,
    )
    .await;
    assert!(file.get("image").is_none(), "{file}");
    let sent = chat(
        &app,
        &owner,
        send(&guild, &channel, json!([wide["id"], file["id"]])),
        StatusCode::OK,
    )
    .await;
    assert_eq!(sent["message"]["attachments"], json!([wide, file]));

    // Files stored before sizes were recorded: no size on the row or message.
    let old = upload_body(
        &app,
        &owner,
        &uri("old.png", "image/png"),
        png(30, 40),
        StatusCode::OK,
    )
    .await;
    let old_message = chat(
        &app,
        &owner,
        send(&guild, &channel, json!([old["id"]])),
        StatusCode::OK,
    )
    .await["message"]
        .clone();
    db.pool
        .get()
        .unwrap()
        .batch_execute(&format!(
            "UPDATE attachments SET image_width=NULL,image_height=NULL; UPDATE messages SET attachments=(SELECT jsonb_agg(e - 'image') FROM jsonb_array_elements(attachments) e) WHERE id='{}'",
            old_message["id"].as_str().unwrap()
        ))
        .unwrap();
    let history = || {
        chat(
            &app,
            &guest,
            json!({"action":"history","guild_id":guild,"channel_id":channel}),
            StatusCode::OK,
        )
    };
    let before = history().await;
    let stored = |history: &Value| {
        history["history"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["id"] == old_message["id"])
            .unwrap()
            .clone()
    };
    assert!(stored(&before)["attachments"][0].get("image").is_none());

    thiscord_backend::chat::start_image_size_backfill(db.pool.clone());
    for _ in 0..100 {
        if crate_count(
            &db,
            "SELECT count(*)::bigint AS count FROM attachments WHERE image_width IS NULL",
        ) == 0
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let after = stored(&history().await);
    assert_eq!(
        after["attachments"][0]["image"],
        json!({"width":30,"height":40})
    );
    // Metadata only: the message is otherwise unchanged and keeps its revision.
    let mut expected = old_message.clone();
    expected["attachments"][0]["image"] = json!({"width":30,"height":40});
    assert_eq!(after, expected);
    assert_eq!(
        crate_count(
            &db,
            "SELECT count(*)::bigint AS count FROM attachments WHERE image_width=160 AND image_height=90"
        ),
        1
    );
    assert_eq!(
        crate_count(
            &db,
            "SELECT count(*)::bigint AS count FROM attachments WHERE image_width=0 AND image_height=0"
        ),
        1
    );
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

/// Lets one sent row stand for gigabytes without storing them: this test schema
/// drops the table's size/data checks and records an inflated `size_bytes`.
pub(super) fn inflate(db: &Database, table: &str, id: &Value, size: u64) {
    let mut c = db.pool.get().unwrap();
    c.batch_execute(&format!(
        "DO $$ DECLARE r record; BEGIN FOR r IN SELECT conname FROM pg_constraint WHERE conrelid='{table}'::regclass AND contype='c' LOOP EXECUTE format('ALTER TABLE {table} DROP CONSTRAINT %I', r.conname); END LOOP; END $$"
    ))
    .unwrap();
    let updated = diesel::sql_query(format!(
        "UPDATE {table} SET size_bytes=$2::bigint WHERE id=$1::uuid AND message_id IS NOT NULL"
    ))
    .bind::<Text, _>(id.as_str().unwrap())
    .bind::<Text, _>(size.to_string())
    .execute(&mut c)
    .unwrap();
    assert_eq!(updated, 1);
}

/// Holds an account's attachment quota lock until both requests wait on it, so
/// the requests really overlap; returns their statuses and bodies in order.
async fn contend(
    db: &Database,
    lock: String,
    requests: [(&'static str, String, Value); 2],
) -> Vec<(StatusCode, Value)> {
    #[derive(QueryableByName)]
    struct Waiting {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        waiting: i64,
    }
    let mut holder = db.pool.get().unwrap();
    holder.batch_execute("BEGIN").unwrap();
    diesel::sql_query("SELECT pg_advisory_xact_lock(hashtextextended($1,0))")
        .bind::<Text, _>(&lock)
        .execute(&mut holder)
        .unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let tasks: Vec<_> = requests
        .into_iter()
        .map(|(path, token, body)| {
            let app = app.clone();
            tokio::spawn(async move { call(&app, path, &token, body).await })
        })
        .collect();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let waiting = diesel::sql_query("SELECT count(*)::bigint AS waiting FROM pg_locks WHERE locktype='advisory' AND NOT granted AND ((classid::bigint<<32)|objid::bigint)=hashtextextended($1,0)")
            .bind::<Text, _>(&lock)
            .get_result::<Waiting>(&mut db.pool.get().unwrap())
            .unwrap()
            .waiting;
        if waiting == 2 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "requests never contended for the quota lock ({waiting} waiting)"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    holder.batch_execute("COMMIT").unwrap();
    let mut results = Vec::new();
    for task in tasks {
        results.push(task.await.unwrap());
    }
    results
}

/// Exactly one of two contended sends wins; the loser sees the quota message.
pub(super) fn assert_one_refused(results: &[(StatusCode, Value)], place: &str) {
    let statuses: Vec<_> = results.iter().map(|(s, _)| *s).collect();
    assert!(
        statuses == [StatusCode::OK, StatusCode::CONFLICT]
            || statuses == [StatusCode::CONFLICT, StatusCode::OK],
        "{results:?}"
    );
    let (_, body) = results
        .iter()
        .find(|(s, _)| *s == StatusCode::CONFLICT)
        .unwrap();
    assert_storage_full(body, place);
}

pub(super) fn assert_storage_full(body: &Value, place: &str) {
    assert_eq!(body["code"], "limit_exceeded", "{body}");
    let message = body["message"].as_str().unwrap();
    assert!(
        message.starts_with(&format!("Your attachment storage {place} is full (2 GiB)"))
            && message.ends_with("Delete older messages with attachments to free space"),
        "{message}"
    );
}

/// Sum of an account's sent attachment sizes, as the quota counts them.
pub(super) fn published_total(
    db: &Database,
    table: &str,
    owner: &str,
    scope: &str,
    id: &Value,
) -> u64 {
    #[derive(QueryableByName)]
    struct Total {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        total: i64,
    }
    diesel::sql_query(format!("SELECT COALESCE(sum(size_bytes),0)::bigint AS total FROM {table} WHERE owner_id=$1::uuid AND {scope}=$2::uuid AND message_id IS NOT NULL"))
        .bind::<Text, _>(owner)
        .bind::<Text, _>(id.as_str().unwrap())
        .get_result::<Total>(&mut db.pool.get().unwrap())
        .unwrap()
        .total as u64
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn published_attachments_are_capped_per_account_per_guild() {
    #[derive(QueryableByName)]
    struct Owner {
        #[diesel(sql_type = Text)]
        id: String,
    }
    let _guard = TEST_LOCK.lock().await;
    let cap = MAX_PUBLISHED_ATTACHMENT_BYTES;
    assert_eq!(cap, 2 << 30);
    let db = database();
    let (app, owner, guest, _, mut state, channel) = setup(&db).await;
    let owner_id = diesel::sql_query("SELECT id::text FROM accounts WHERE username='owner'")
        .get_result::<Owner>(&mut db.pool.get().unwrap())
        .unwrap()
        .id;
    let guild = state["guild"]["id"].clone();
    let uri = upload_url(&guild, &channel, "file.bin", "text/plain");
    let one = |token: &str, uri: &str| {
        let (app, token, uri) = (app.clone(), token.to_owned(), uri.to_owned());
        async move {
            upload_body(&app, &token, &uri, Body::from("x"), StatusCode::OK).await["id"].clone()
        }
    };

    // An older sent attachment leaves room for exactly two more bytes.
    let base = one(&owner, &uri).await;
    let sent = chat(
        &app,
        &owner,
        send(&guild, &channel, json!([base])),
        StatusCode::OK,
    )
    .await;
    inflate(&db, "attachments", &base, cap - 2);
    let a = one(&owner, &uri).await;
    chat(
        &app,
        &owner,
        send(&guild, &channel, json!([a])),
        StatusCode::OK,
    )
    .await;
    let (b, c) = (one(&owner, &uri).await, one(&owner, &uri).await);
    // Sends succeed up to the cap and fail past it, leaving the file staged.
    chat(
        &app,
        &owner,
        send(&guild, &channel, json!([b])),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        published_total(&db, "attachments", &owner_id, "guild_id", &guild),
        cap
    );
    let messages = crate_count(&db, "SELECT count(*)::bigint AS count FROM messages");
    let refused = chat(
        &app,
        &owner,
        send(&guild, &channel, json!([c])),
        StatusCode::CONFLICT,
    )
    .await;
    assert_storage_full(&refused, "on this server");
    assert_eq!(
        crate_count(&db, "SELECT count(*)::bigint AS count FROM messages"),
        messages
    );
    assert_eq!(
        crate_count(
            &db,
            "SELECT count(*)::bigint AS count FROM attachments WHERE message_id IS NULL"
        ),
        1
    );
    // At the cap, uploads are refused before the body is buffered.
    let early = upload_body(&app, &owner, &uri, Body::from("x"), StatusCode::CONFLICT).await;
    assert_storage_full(&early, "on this server");

    // Another member of the same guild is unaffected.
    let theirs = one(&guest, &uri).await;
    chat(
        &app,
        &guest,
        send(&guild, &channel, json!([theirs])),
        StatusCode::OK,
    )
    .await;
    // The same account in another guild is unaffected.
    let mut other = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"elsewhere"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &owner,
        &mut other,
        json!({"action":"create_channel","name":"two","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let (other_guild, other_channel) = (
        other["guild"]["id"].clone(),
        other["channels"][0]["id"].clone(),
    );
    let elsewhere = one(
        &owner,
        &upload_url(&other_guild, &other_channel, "file.bin", "text/plain"),
    )
    .await;
    chat(
        &app,
        &owner,
        send(&other_guild, &other_channel, json!([elsewhere])),
        StatusCode::OK,
    )
    .await;

    // Deleting a message frees its attachments' quota.
    chat(&app, &owner, json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":sent["message"]["id"],"revision":sent["message"]["revision"]}), StatusCode::OK).await;
    chat(
        &app,
        &owner,
        send(&guild, &channel, json!([c])),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        published_total(&db, "attachments", &owner_id, "guild_id", &guild),
        3
    );

    // Sends in different channels hold different channel locks; the account
    // lock still keeps both from taking the last byte.
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"three","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let second = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] != channel)
        .unwrap()["id"]
        .clone();
    inflate(&db, "attachments", &a, cap - 3);
    let d = one(&owner, &uri).await;
    let e = one(
        &owner,
        &upload_url(&guild, &second, "file.bin", "text/plain"),
    )
    .await;
    let results = contend(
        &db,
        format!("attachment:{owner_id}"),
        [
            (CHAT_PATH, owner.clone(), send(&guild, &channel, json!([d]))),
            (CHAT_PATH, owner.clone(), send(&guild, &second, json!([e]))),
        ],
    )
    .await;
    assert_one_refused(&results, "on this server");
    assert_eq!(
        published_total(&db, "attachments", &owner_id, "guild_id", &guild),
        cap
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn uploads_wait_for_the_channel_before_taking_the_account_quota_lock() {
    #[derive(QueryableByName)]
    struct Flag {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        flag: bool,
    }
    #[derive(QueryableByName)]
    struct Pid {
        #[diesel(sql_type = diesel::sql_types::Integer)]
        pid: i32,
    }
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, _, _, state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let lock = format!(
        "attachment:{}",
        diesel::sql_query("SELECT id::text AS data FROM accounts WHERE username='owner'")
            .get_result::<JsonText>(&mut db.pool.get().unwrap())
            .unwrap()
            .data
    );
    // A send holds its channel FOR UPDATE and then takes the account lock in
    // `attach`; an upload must not hold that account lock while it waits here.
    let mut sender = db.pool.get().unwrap();
    sender.batch_execute("BEGIN").unwrap();
    diesel::sql_query("SELECT id FROM channels WHERE id=$1::uuid FOR UPDATE")
        .bind::<Text, _>(channel.as_str().unwrap())
        .execute(&mut sender)
        .unwrap();
    let sender_pid = diesel::sql_query("SELECT pg_backend_pid() AS pid")
        .get_result::<Pid>(&mut sender)
        .unwrap()
        .pid;
    let uri = upload_url(&guild, &channel, "file.bin", "text/plain");
    let upload = {
        let (app, owner) = (app.clone(), owner.clone());
        tokio::spawn(async move {
            upload_body(&app, &owner, &uri, Body::from("x"), StatusCode::OK).await
        })
    };
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !diesel::sql_query("SELECT EXISTS(SELECT 1 FROM pg_stat_activity WHERE $1::integer=ANY(pg_blocking_pids(pid))) AS flag")
        .bind::<diesel::sql_types::Integer, _>(sender_pid)
        .get_result::<Flag>(&mut db.pool.get().unwrap())
        .unwrap()
        .flag
    {
        assert!(std::time::Instant::now() < deadline, "upload never waited for the channel");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut probe = db.pool.get().unwrap();
    let free = probe
        .transaction::<_, diesel::result::Error, _>(|c| {
            diesel::sql_query("SELECT pg_try_advisory_xact_lock(hashtextextended($1,0)) AS flag")
                .bind::<Text, _>(&lock)
                .get_result::<Flag>(c)
                .map(|f| f.flag)
        })
        .unwrap();
    sender.batch_execute("COMMIT").unwrap();
    assert!(
        free,
        "upload held the account lock while waiting for the channel"
    );
    upload.await.unwrap();
}
#[derive(QueryableByName)]
struct JsonText {
    #[diesel(sql_type = Text)]
    data: String,
}
