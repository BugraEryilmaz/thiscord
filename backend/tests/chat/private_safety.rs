use super::friends::{event, snapshot, social, socket};
use super::*;
use std::time::Duration;
use thiscord_shared::{friends::DIRECT_ATTACHMENTS_PATH, safety::SAFETY_PATH};

fn pair(db: &Database, a: AccountId, b: AccountId) -> Uuid {
    let id = Uuid::new_v4();
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'accepted')").bind::<Text,_>(id.to_string()).bind::<Text,_>(a.to_string()).bind::<Text,_>(b.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    id
}
async fn safety(app: &Router, token: &str, value: Value, status: StatusCode) -> Value {
    let (got, body) = call(app, SAFETY_PATH, token, value).await;
    assert_eq!(got, status, "{body}");
    body
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn private_search_tracks_blocking_restored_consent_and_deletion() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "search_safety_a");
    let (bid, b) = user(&db, "search_safety_b");
    let (cid, c) = user(&db, "search_safety_c");
    let conversation = pair(&db, aid, bid);
    let unrelated = pair(&db, aid, cid);
    let app = api::router(Some(db.pool.clone()), vec![]);
    let message = social(&app, &b, json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"needle retained history"}), StatusCode::OK).await["message"].clone();
    social(&app, &c, json!({"action":"send","conversation_id":unrelated,"client_id":Uuid::new_v4(),"content":"needle other friend"}), StatusCode::OK).await;
    let path = thiscord_shared::search::SEARCH_PATH;
    let scope = json!({"kind":"private","conversation_id":conversation});
    let search = json!({"action":"search","filters":{"scope":scope,"query":"needle","person":null,"after":null,"before":null}});
    let context = json!({"action":"context","scope":scope,"message_id":message["id"]});
    let mut broad = search.clone();
    broad["filters"]["scope"]["conversation_id"] = Value::Null;
    for token in [&a, &b] {
        assert_eq!(
            call(&app, path, token, search.clone()).await.1["messages"][0]["id"],
            message["id"]
        );
        assert_eq!(
            call(&app, path, token, context.clone()).await.0,
            StatusCode::OK
        );
    }
    safety(
        &app,
        &a,
        json!({"action":"block","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    for token in [&a, &b] {
        assert_eq!(
            call(&app, path, token, search.clone()).await.0,
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            call(&app, path, token, context.clone()).await.0,
            StatusCode::FORBIDDEN
        );
    }
    let (status, visible) = call(&app, path, &a, broad).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(visible["messages"].as_array().unwrap().len(), 1);
    assert_eq!(
        visible["messages"][0]["scope"]["conversation_id"],
        json!(unrelated)
    );
    safety(
        &app,
        &a,
        json!({"action":"unblock","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        call(&app, path, &a, search.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        call(&app, path, &b, context.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    social(
        &app,
        &a,
        json!({"action":"add","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        call(&app, path, &a, search.clone()).await.0,
        StatusCode::FORBIDDEN
    );
    social(
        &app,
        &b,
        json!({"action":"accept","account_id":aid}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        call(&app, path, &a, search.clone()).await.1["messages"][0]["id"],
        message["id"]
    );
    social(&app, &b, json!({"action":"delete","conversation_id":conversation,"message_id":message["id"],"revision":message["revision"]}), StatusCode::OK).await;
    for token in [&a, &b] {
        let (status, result) = call(&app, path, token, search.clone()).await;
        assert_eq!(status, StatusCode::OK);
        assert!(result["messages"].as_array().unwrap().is_empty());
        assert_eq!(
            call(&app, path, token, context.clone()).await.0,
            StatusCode::FORBIDDEN
        );
    }
}
async fn serve(app: &Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = app.clone();
    (
        address,
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() }),
    )
}
async fn revoked(socket: &mut Socket) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match socket.next().await {
                Some(Ok(WsMessage::Text(text))) => {
                    let v: Value = serde_json::from_str(&text).unwrap();
                    if v["event"]["type"] == "revoked" {
                        break;
                    }
                }
                Some(Ok(WsMessage::Close(_))) | None => break,
                _ => {}
            }
        }
    })
    .await
    .expect("private socket must be revoked");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn private_revisions_tombstones_retries_history_and_typing() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "edit_a");
    let (bid, b) = user(&db, "edit_b");
    let (_, outsider) = user(&db, "edit_out");
    let conversation = pair(&db, aid, bid);
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let (address, server) = serve(&app).await;
    let mut reader = socket(address, &b).await;
    let mut writer = socket(address, &a).await;
    event(
        &mut reader,
        json!({"type":"select","conversation_id":conversation}),
    )
    .await;
    event(
        &mut writer,
        json!({"type":"select","conversation_id":conversation}),
    )
    .await;
    snapshot(&mut reader, |s| s["conversation_id"] == json!(conversation)).await;
    let send = json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"original"});
    let m = social(&app, &a, send.clone(), StatusCode::OK).await["message"].clone();
    assert_eq!(m["revision"], 1);
    snapshot(&mut reader, |s| {
        s["history"]["messages"]
            .as_array()
            .is_some_and(|v| v.len() == 1)
    })
    .await;
    let edit = json!({"action":"edit","conversation_id":conversation,"message_id":m["id"],"revision":1,"content":"edited"});
    social(&app, &b, edit.clone(), StatusCode::FORBIDDEN).await;
    social(&app, &outsider, edit.clone(), StatusCode::FORBIDDEN).await;
    let (one, two) = tokio::join!(
        social(&app, &a, edit.clone(), StatusCode::OK),
        social(&app, &a, edit.clone(), StatusCode::OK)
    );
    assert_eq!(one, two);
    assert_eq!(one["message"]["revision"], 2);
    let live = snapshot(&mut reader, |s| {
        s["history"]["messages"][0]["revision"] == 2
    })
    .await;
    assert_eq!(live["history"]["messages"][0]["content"], "edited");
    assert_eq!(
        social(&app, &a, send.clone(), StatusCode::OK).await["message"]["revision"],
        2
    );
    let mut stale = edit;
    stale["content"] = json!("stale overwrite");
    social(&app, &a, stale, StatusCode::CONFLICT).await;
    event(&mut writer, json!({"type":"typing","active":true})).await;
    snapshot(&mut reader, |s| s["typing"] == json!([aid])).await;
    snapshot(&mut reader, |s| s["typing"] == json!([])).await; // Four-second expiry without a stop frame.
    event(&mut writer, json!({"type":"typing","active":true})).await;
    snapshot(&mut reader, |s| s["typing"] == json!([aid])).await;
    writer.close(None).await.unwrap();
    snapshot(&mut reader, |s| s["typing"] == json!([])).await;
    let deletion =
        json!({"action":"delete","conversation_id":conversation,"message_id":m["id"],"revision":2});
    social(&app, &b, deletion.clone(), StatusCode::FORBIDDEN).await;
    let deleted = social(&app, &a, deletion.clone(), StatusCode::OK).await;
    assert_eq!(deleted["message"]["deleted"], true);
    assert_eq!(deleted["message"]["content"], "");
    assert_eq!(deleted["message"]["revision"], 3);
    assert_eq!(social(&app, &a, deletion, StatusCode::OK).await, deleted);
    assert_eq!(social(&app, &a, send, StatusCode::OK).await, deleted);
    snapshot(&mut reader, |s| {
        s["history"]["messages"][0]["deleted"] == true
    })
    .await;
    let mut fresh = socket(address, &b).await;
    event(
        &mut fresh,
        json!({"type":"select","conversation_id":conversation}),
    )
    .await;
    let snap = snapshot(&mut fresh, |s| s["conversation_id"] == json!(conversation)).await;
    assert_eq!(snap["history"]["messages"][0]["deleted"], true);
    assert_eq!(snap["friends"][0]["unread_count"], 0);
    assert_eq!(snap["typing"], json!([]));
    // Older-page subscriptions receive edits too, without mixing the live window.
    let newer=social(&app,&a,json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"newer"}),StatusCode::OK).await["message"].clone();
    let at = serde_json::from_value(newer["created_at"].clone()).unwrap();
    let id = Uuid::parse_str(newer["id"].as_str().unwrap()).unwrap();
    let before = thiscord_shared::pagination::PageCursor::new(at, id);
    event(
        &mut fresh,
        json!({"type":"select","conversation_id":conversation,"before":before}),
    )
    .await;
    let past = snapshot(&mut fresh, |s| s["before"] == json!(before)).await;
    assert_eq!(past["history"]["messages"].as_array().unwrap().len(), 1);
    assert_eq!(past["history"]["messages"][0]["deleted"], true);
    fresh.close(None).await.unwrap();
    server.abort();
}
async fn upload(
    app: &Router,
    token: &str,
    conversation: Uuid,
    bytes: Vec<u8>,
    status: StatusCode,
) -> Value {
    let uri = format!(
        "{DIRECT_ATTACHMENTS_PATH}?conversation_id={conversation}&filename=test.png&content_type=image%2Fpng"
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(bytes))
                .unwrap(),
        )
        .await
        .unwrap();
    let got = response.status();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 32768).await.unwrap()).unwrap();
    assert_eq!(got, status, "{body}");
    body
}
async fn download(
    app: &Router,
    token: &str,
    id: &Value,
    status: StatusCode,
) -> axum::response::Response {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!(
                    "{DIRECT_ATTACHMENTS_PATH}/{}",
                    id.as_str().unwrap()
                ))
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), status);
    assert_eq!(response.headers()["cache-control"], "no-store");
    response
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn private_attachment_ownership_lifetime_and_revoked_streams() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "file_a");
    let (bid, b) = user(&db, "file_b");
    let (oid, out) = user(&db, "file_out");
    let conversation = pair(&db, aid, bid);
    let other = pair(&db, aid, oid);
    let app = api::router(Some(db.pool.clone()), vec![]);
    upload(&app, &out, conversation, vec![1], StatusCode::FORBIDDEN).await;
    upload(&app, &a, conversation, vec![], StatusCode::BAD_REQUEST).await;
    upload(
        &app,
        &a,
        conversation,
        vec![0; MAX_ATTACHMENT_BYTES + 1],
        StatusCode::BAD_REQUEST,
    )
    .await;
    let file = upload(&app, &a, conversation, vec![42; 128 * 1024], StatusCode::OK).await;
    assert_eq!(file["content_type"], "application/octet-stream");
    drop(download(&app, &a, &file["id"], StatusCode::FORBIDDEN).await); // Stage is not published even to its owner.
    let send = json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"","attachments":[file["id"]]});
    social(&app, &b, send.clone(), StatusCode::FORBIDDEN).await;
    let mut foreign = send.clone();
    foreign["conversation_id"] = json!(other);
    social(&app, &a, foreign, StatusCode::FORBIDDEN).await;
    let m = social(&app, &a, send.clone(), StatusCode::OK).await["message"].clone();
    assert_eq!(m["attachments"][0], file);
    assert_eq!(
        social(&app, &a, send.clone(), StatusCode::OK).await["message"],
        m
    );
    drop(download(&app, &out, &file["id"], StatusCode::FORBIDDEN).await);
    let bytes = to_bytes(
        download(&app, &b, &file["id"], StatusCode::OK)
            .await
            .into_body(),
        MAX_ATTACHMENT_BYTES,
    )
    .await
    .unwrap();
    assert_eq!(bytes.len(), 128 * 1024);
    let held = download(&app, &b, &file["id"], StatusCode::OK).await;
    social(
        &app,
        &a,
        json!({"action":"delete","conversation_id":conversation,"message_id":m["id"],"revision":1}),
        StatusCode::OK,
    )
    .await;
    assert!(
        to_bytes(held.into_body(), MAX_ATTACHMENT_BYTES)
            .await
            .is_err()
    );
    drop(download(&app, &a, &file["id"], StatusCode::FORBIDDEN).await);
    assert_eq!(
        social(&app, &a, send, StatusCode::OK).await["message"]["attachments"],
        json!([])
    );
    let next = upload(&app, &b, conversation, vec![1, 2, 3], StatusCode::OK).await;
    let sent=social(&app,&b,json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"file","attachments":[next["id"]]}),StatusCode::OK).await;
    assert_eq!(sent["message"]["attachments"][0], next);
    let held = download(&app, &a, &next["id"], StatusCode::OK).await;
    safety(
        &app,
        &b,
        json!({"action":"block","account_id":aid}),
        StatusCode::OK,
    )
    .await;
    assert!(
        to_bytes(held.into_body(), MAX_ATTACHMENT_BYTES)
            .await
            .is_err()
    );
    drop(download(&app, &b, &next["id"], StatusCode::FORBIDDEN).await);
    safety(
        &app,
        &b,
        json!({"action":"unblock","account_id":aid}),
        StatusCode::OK,
    )
    .await;
    drop(download(&app, &b, &next["id"], StatusCode::FORBIDDEN).await);
}
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn blocks_serialize_consent_revoke_presence_calls_and_never_restore_on_unblock() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "block_a");
    let (bid, b) = user(&db, "block_b");
    let conversation = pair(&db, aid, bid);
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let (address, server) = serve(&app).await;
    let mut sa = socket(address, &a).await;
    let mut sb = socket(address, &b).await;
    snapshot(&mut sa, |s| s["friends"][0]["online"] == true).await;
    let call = social(
        &app,
        &a,
        json!({"action":"start_call","conversation_id":conversation}),
        StatusCode::OK,
    )
    .await["call"]["id"]
        .clone();
    social(
        &app,
        &b,
        json!({"action":"accept_call","call_id":call}),
        StatusCode::OK,
    )
    .await;
    let block = json!({"action":"block","account_id":bid});
    let (r1, r2) = tokio::join!(
        safety(&app, &a, block.clone(), StatusCode::OK),
        safety(
            &app,
            &b,
            json!({"action":"block","account_id":aid}),
            StatusCode::OK
        )
    );
    assert_eq!(r1["result"], "done");
    assert_eq!(r2["result"], "done");
    revoked(&mut sa).await;
    revoked(&mut sb).await;
    let mut new = socket(address, &a).await;
    let snap = snapshot(&mut new, |_| true).await;
    assert_eq!(snap["friends"], json!([]));
    assert_eq!(snap["calls"], json!([]));
    for token in [&a, &b] {
        social(
            &app,
            token,
            json!({"action":"history","conversation_id":conversation}),
            StatusCode::FORBIDDEN,
        )
        .await;
        social(
            &app,
            token,
            json!({"action":"start_call","conversation_id":conversation}),
            StatusCode::FORBIDDEN,
        )
        .await;
        social(
            &app,
            token,
            json!({"action":"accept_call","call_id":call}),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    safety(
        &app,
        &a,
        json!({"action":"unblock","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &a,
        json!({"action":"add","account_id":bid}),
        StatusCode::FORBIDDEN,
    )
    .await;
    // Removing a relationship is not an unblock and cannot clear the other party's block.
    social(
        &app,
        &a,
        json!({"action":"remove","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &b,
        json!({"action":"add","account_id":aid}),
        StatusCode::FORBIDDEN,
    )
    .await;
    safety(
        &app,
        &b,
        json!({"action":"unblock","account_id":aid}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &a,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(
        &app,
        &a,
        json!({"action":"add","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &a,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(
        &app,
        &b,
        json!({"action":"accept","account_id":aid}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &a,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::OK,
    )
    .await;
    // Racing a new request with a block cannot reestablish consent.
    social(
        &app,
        &a,
        json!({"action":"remove","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    let (blocked, (status, _)) = tokio::join!(
        safety(&app, &a, block, StatusCode::OK),
        super::call(
            &app,
            thiscord_shared::friends::FRIENDS_PATH,
            &b,
            json!({"action":"add","account_id":aid})
        )
    );
    assert_eq!(blocked["result"], "done");
    assert!([StatusCode::OK, StatusCode::FORBIDDEN].contains(&status));
    social(
        &app,
        &a,
        json!({"action":"accept","account_id":bid}),
        StatusCode::FORBIDDEN,
    )
    .await;
    new.close(None).await.ok();
    server.abort();
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn reports_capture_only_submitted_evidence_and_enforce_review_scope() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "report_a");
    let (bid, b) = user(&db, "report_b");
    let (_, admin) = user(&db, "report_admin");
    let (_, out) = user(&db, "report_out");
    permissions::bootstrap_owner(&db.pool, "report_admin").unwrap();
    let conversation = pair(&db, aid, bid);
    let app = api::router(Some(db.pool.clone()), vec![]);
    let m=social(&app,&b,json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"reported evidence"}),StatusCode::OK).await["message"].clone();
    social(&app,&b,json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"unrelated private secret"}),StatusCode::OK).await;
    let report = json!({"action":"create_report","id":Uuid::new_v4(),"target":{"kind":"direct_message","conversation_id":conversation,"message_id":m["id"]},"reason":"Please review this message"});
    safety(&app, &out, report.clone(), StatusCode::FORBIDDEN).await;
    let mut forged = report.clone();
    forged["evidence"] = json!({"content":"forged"});
    safety(&app, &a, forged, StatusCode::BAD_REQUEST).await;
    let created = safety(&app, &a, report.clone(), StatusCode::OK).await;
    assert_eq!(
        created["report"]["evidence"]["content"],
        "reported evidence"
    );
    assert_eq!(created["report"]["subject_id"], json!(bid));
    assert_eq!(safety(&app, &a, report, StatusCode::OK).await, created);
    safety(
        &app,
        &b,
        json!({"action":"reports","inbox":{"kind":"mine"}}),
        StatusCode::OK,
    )
    .await["page"]["items"]
        .as_array()
        .unwrap()
        .is_empty()
        .then_some(())
        .unwrap();
    social(
        &app,
        &b,
        json!({"action":"delete","conversation_id":conversation,"message_id":m["id"],"revision":1}),
        StatusCode::OK,
    )
    .await;
    safety(
        &app,
        &a,
        json!({"action":"block","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    let inbox = json!({"action":"reports","inbox":{"kind":"instance"}});
    safety(&app, &a, inbox.clone(), StatusCode::FORBIDDEN).await;
    let review = safety(&app, &admin, inbox, StatusCode::OK).await;
    assert_eq!(review["page"]["items"].as_array().unwrap().len(), 1);
    assert!(!review.to_string().contains("unrelated private secret"));
    assert_eq!(
        review["page"]["items"][0]["evidence"]["content"],
        "reported evidence"
    );
    social(
        &app,
        &admin,
        json!({"action":"history","conversation_id":conversation}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let resolve = json!({"action":"resolve","id":created["report"]["id"],"revision":1,"state":"resolved","resolution":"Reviewed submitted evidence","guild_revision":null});
    safety(&app, &a, resolve.clone(), StatusCode::FORBIDDEN).await;
    let done = safety(&app, &admin, resolve.clone(), StatusCode::OK).await;
    assert_eq!(done["report"]["revision"], 2);
    assert_eq!(
        safety(&app, &admin, resolve.clone(), StatusCode::OK).await,
        done
    );
    let mut conflict = resolve;
    conflict["state"] = json!("dismissed");
    safety(&app, &admin, conflict, StatusCode::CONFLICT).await;
    for _ in 0..4 {
        safety(&app,&a,json!({"action":"create_report","id":Uuid::new_v4(),"target":{"kind":"user","account_id":bid},"reason":"Account report"}),StatusCode::OK).await;
    }
    safety(&app,&a,json!({"action":"create_report","id":Uuid::new_v4(),"target":{"kind":"user","account_id":bid},"reason":"Account report"}),StatusCode::TOO_MANY_REQUESTS).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn guild_reports_require_channel_moderation_and_locked_guild_revision() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, state, channel) = setup(&db).await;
    let (admin_id, admin) = user(&db, "outside_admin");
    diesel::sql_query("INSERT INTO instance_admins(account_id) VALUES($1::uuid)")
        .bind::<Text, _>(admin_id.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let guild = state["guild"]["id"].clone();
    let m=chat(&app,&owner,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"guild evidence"}),StatusCode::OK).await["message"].clone();
    let create = json!({"action":"create_report","id":Uuid::new_v4(),"target":{"kind":"guild_message","guild_id":guild,"channel_id":channel,"message_id":m["id"]},"reason":"Please moderate"});
    safety(&app, &admin, create.clone(), StatusCode::FORBIDDEN).await;
    let report = safety(&app, &guest, create, StatusCode::OK).await["report"].clone();
    let inbox =
        json!({"action":"reports","inbox":{"kind":"guild","guild_id":guild,"channel_id":channel}});
    safety(&app, &admin, inbox.clone(), StatusCode::FORBIDDEN).await;
    safety(&app, &guest, inbox.clone(), StatusCode::FORBIDDEN).await;
    let visible = safety(&app, &owner, inbox, StatusCode::OK).await;
    assert_eq!(visible["page"]["items"][0]["id"], report["id"]);
    let instance = safety(
        &app,
        &admin,
        json!({"action":"reports","inbox":{"kind":"instance"}}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(instance["page"]["items"], json!([]));
    let mut resolve = json!({"action":"resolve","id":report["id"],"revision":1,"state":"dismissed","resolution":"No action needed","guild_revision":null});
    safety(&app, &owner, resolve.clone(), StatusCode::BAD_REQUEST).await;
    resolve["guild_revision"] = json!(0);
    safety(&app, &owner, resolve.clone(), StatusCode::CONFLICT).await;
    resolve["guild_revision"] = state["guild"]["revision"].clone();
    safety(&app, &admin, resolve.clone(), StatusCode::FORBIDDEN).await;
    let resolved = safety(&app, &owner, resolve, StatusCode::OK).await;
    assert_eq!(resolved["report"]["state"], "dismissed");
    let mine = safety(
        &app,
        &guest,
        json!({"action":"reports","inbox":{"kind":"mine"}}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(mine["page"]["items"][0]["state"], "dismissed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn private_upload_quotas_expiry_and_claims_are_atomic() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "quota_a");
    let (bid, _) = user(&db, "quota_b");
    let (cid, _) = user(&db, "quota_c");
    let one = pair(&db, aid, bid);
    let two = pair(&db, aid, cid);
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut first = None;
    for _ in 0..16 {
        let file = upload(&app, &a, one, vec![1, 2, 3], StatusCode::OK).await;
        first.get_or_insert(file);
    }
    upload(&app, &a, two, vec![1], StatusCode::TOO_MANY_REQUESTS).await;
    diesel::sql_query("UPDATE direct_attachments SET expires_at=now()-interval '1 second'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let stale = first.unwrap();
    social(&app,&a,json!({"action":"send","conversation_id":one,"client_id":Uuid::new_v4(),"content":"expired","attachments":[stale["id"]]}),StatusCode::FORBIDDEN).await;
    let fresh = upload(&app, &a, one, vec![1], StatusCode::OK).await;
    let count: Vec<Value> =
        diesel::sql_query("SELECT to_jsonb(count(*)) AS data FROM direct_attachments")
            .load::<JsonRow>(&mut db.pool.get().unwrap())
            .unwrap()
            .into_iter()
            .map(|r| r.data)
            .collect();
    assert_eq!(count, vec![json!(1)]);
    social(&app,&a,json!({"action":"send","conversation_id":one,"client_id":Uuid::new_v4(),"content":"duplicate","attachments":[fresh["id"],fresh["id"]]}),StatusCode::BAD_REQUEST).await;
    let c1 = json!({"action":"send","conversation_id":one,"client_id":Uuid::new_v4(),"content":"one","attachments":[fresh["id"]]});
    let c2 = json!({"action":"send","conversation_id":one,"client_id":Uuid::new_v4(),"content":"two","attachments":[fresh["id"]]});
    let (r1, r2) = tokio::join!(
        super::call(&app, thiscord_shared::friends::FRIENDS_PATH, &a, c1),
        super::call(&app, thiscord_shared::friends::FRIENDS_PATH, &a, c2)
    );
    assert!(
        (r1.0 == StatusCode::OK && r2.0 == StatusCode::FORBIDDEN)
            || (r2.0 == StatusCode::OK && r1.0 == StatusCode::FORBIDDEN)
    );
}
#[derive(diesel::QueryableByName)]
struct JsonRow {
    #[diesel(sql_type=diesel::sql_types::Jsonb)]
    data: Value,
}
