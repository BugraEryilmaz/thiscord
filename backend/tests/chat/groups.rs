//! Group conversations: membership rules, isolation, revocation and calls.
use super::friends::{snapshot, social, socket};
use super::*;
use thiscord_shared::{friends::DIRECT_ATTACHMENTS_PATH, safety::SAFETY_PATH, search::SEARCH_PATH};

fn befriend(db: &Database, a: AccountId, b: AccountId) {
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES(gen_random_uuid(),LEAST($1::uuid,$2::uuid),GREATEST($1::uuid,$2::uuid),$1::uuid,'accepted')")
        .bind::<Text, _>(a.to_string())
        .bind::<Text, _>(b.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
}
fn block(db: &Database, blocker: AccountId, blocked: AccountId) {
    diesel::sql_query(
        "INSERT INTO account_blocks(blocker_id,blocked_id) VALUES($1::uuid,$2::uuid)",
    )
    .bind::<Text, _>(blocker.to_string())
    .bind::<Text, _>(blocked.to_string())
    .execute(&mut db.pool.get().unwrap())
    .unwrap();
}
async fn history(app: &Router, token: &str, group: &Value, status: StatusCode) -> Vec<Value> {
    let body = social(
        app,
        token,
        json!({"action":"history","conversation_id":group}),
        status,
    )
    .await;
    body["history"]["messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}
fn kinds(messages: &[Value]) -> Vec<String> {
    messages
        .iter()
        .map(|m| {
            m["system"]["type"]
                .as_str()
                .map_or_else(|| "message".to_owned(), str::to_owned)
        })
        .collect()
}
async fn upload(app: &Router, token: &str, conversation: &Value, status: StatusCode) -> Value {
    let uri = format!(
        "{DIRECT_ATTACHMENTS_PATH}?conversation_id={}&filename=group.png&content_type=image%2Fpng",
        conversation.as_str().unwrap()
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(b"\x89PNG\r\n\x1a\ngroup".to_vec()))
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
async fn download(app: &Router, token: &str, id: &Value) -> StatusCode {
    app.clone()
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
        .unwrap()
        .status()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn group_membership_isolation_and_revocation() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "group_owner");
    let (bid, b) = user(&db, "group_bob");
    let (cid, c) = user(&db, "group_carol");
    let (did, d) = user(&db, "group_dave");
    let (eid, _) = user(&db, "group_erin");
    let (_, out) = user(&db, "group_outsider");
    for other in [bid, cid, did, eid] {
        befriend(&db, aid, other);
    }
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });

    // Only accepted friends of the creator can be added.
    let outsider_id = AccountId::from_uuid(Uuid::new_v4());
    social(
        &app,
        &a,
        json!({"action":"create_group","client_id":Uuid::new_v4(),"members":[bid,outsider_id]}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    social(
        &app,
        &b,
        json!({"action":"create_group","client_id":Uuid::new_v4(),"members":[cid]}),
        StatusCode::BAD_REQUEST,
    )
    .await;

    // Creation is idempotent per client UUID; a changed payload conflicts.
    let client = Uuid::new_v4();
    let created = social(
        &app,
        &a,
        json!({"action":"create_group","client_id":client,"members":[bid,cid],"name":"  Trip  "}),
        StatusCode::OK,
    )
    .await;
    let group = created["group"]["conversation_id"].clone();
    assert_eq!(created["group"]["name"], "Trip");
    assert_eq!(created["group"]["members"].as_array().unwrap().len(), 3);
    assert_eq!(created["group"]["members"][0]["person"]["id"], json!(aid));
    assert_eq!(created["group"]["members"][0]["owner"], true);
    let retried = social(
        &app,
        &a,
        json!({"action":"create_group","client_id":client,"members":[cid,bid],"name":"Trip"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(retried["group"]["conversation_id"], group);
    social(
        &app,
        &a,
        json!({"action":"create_group","client_id":client,"members":[bid],"name":"Trip"}),
        StatusCode::CONFLICT,
    )
    .await;

    // Members read and write; outsiders cannot read, write, upload or select.
    let mut carol = socket(address, &c).await;
    let mut stranger = socket(address, &out).await;
    let initial = snapshot(&mut carol, |s| {
        s["groups"].as_array().is_some_and(|g| g.len() == 1)
    })
    .await;
    assert_eq!(initial["groups"][0]["unread_count"], 0);
    assert!(
        snapshot(&mut stranger, |_| true)
            .await
            .get("groups")
            .is_none()
    );
    let message = Uuid::new_v4();
    let sent = social(&app, &b, json!({"action":"send","conversation_id":group,"client_id":message,"content":"hello @group_carol"}), StatusCode::OK).await;
    let again = social(&app, &b, json!({"action":"send","conversation_id":group,"client_id":message,"content":"hello @group_carol"}), StatusCode::OK).await;
    assert_eq!(sent["message"]["id"], again["message"]["id"]);
    social(
        &app,
        &b,
        json!({"action":"send","conversation_id":group,"client_id":message,"content":"different"}),
        StatusCode::CONFLICT,
    )
    .await;
    snapshot(&mut carol, |s| s["groups"][0]["unread_count"] == 1).await;
    history(&app, &out, &group, StatusCode::FORBIDDEN).await;
    social(
        &app,
        &out,
        json!({"action":"send","conversation_id":group,"client_id":Uuid::new_v4(),"content":"x"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    upload(&app, &out, &group, StatusCode::FORBIDDEN).await;
    super::friends::event(
        &mut stranger,
        json!({"type":"select","conversation_id":group}),
    )
    .await;
    let selected = snapshot(&mut stranger, |_| true).await;
    assert!(selected["conversation_id"].is_null() && selected["history"].is_null());
    assert_eq!(
        kinds(&history(&app, &c, &group, StatusCode::OK).await),
        ["created", "message"]
    );
    // System notices cannot be edited, deleted or reported.
    let notice = history(&app, &a, &group, StatusCode::OK).await[0].clone();
    social(&app, &a, json!({"action":"edit","conversation_id":group,"message_id":notice["id"],"revision":1,"content":"x"}), StatusCode::FORBIDDEN).await;
    social(
        &app,
        &a,
        json!({"action":"delete","conversation_id":group,"message_id":notice["id"],"revision":1}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let reported = call(&app, SAFETY_PATH, &b, json!({"action":"create_report","id":Uuid::new_v4(),"target":{"kind":"direct_message","conversation_id":group,"message_id":notice["id"]},"reason":"notice"})).await;
    assert_eq!(reported.0, StatusCode::FORBIDDEN, "{}", reported.1);

    // Attachments follow membership.
    let file = upload(&app, &b, &group, StatusCode::OK).await;
    let with_file = social(&app, &b, json!({"action":"send","conversation_id":group,"client_id":Uuid::new_v4(),"content":"","attachments":[file["id"]]}), StatusCode::OK).await;
    assert_eq!(download(&app, &c, &file["id"]).await, StatusCode::OK);
    assert_eq!(
        download(&app, &out, &file["id"]).await,
        StatusCode::FORBIDDEN
    );

    // Metadata writes check the submitted revision; only the owner removes.
    let revision = created["group"]["revision"].as_i64().unwrap();
    social(&app, &c, json!({"action":"rename_group","conversation_id":group,"revision":revision + 7,"name":"Late"}), StatusCode::CONFLICT).await;
    let renamed = social(&app, &c, json!({"action":"rename_group","conversation_id":group,"revision":revision,"name":"Weekend"}), StatusCode::OK).await;
    let revision = renamed["group"]["revision"].as_i64().unwrap();
    // An exact repeat of the rename is idempotent.
    social(&app, &c, json!({"action":"rename_group","conversation_id":group,"revision":revision - 1,"name":"Weekend"}), StatusCode::OK).await;
    social(&app, &b, json!({"action":"remove_group_member","conversation_id":group,"revision":revision,"account_id":cid}), StatusCode::FORBIDDEN).await;

    // Additions: friends of the adder only, never across a block with a member.
    social(&app, &b, json!({"action":"add_group_members","conversation_id":group,"revision":revision,"members":[did]}), StatusCode::BAD_REQUEST).await;
    block(&db, did, cid);
    let refused = social(&app, &a, json!({"action":"add_group_members","conversation_id":group,"revision":revision,"members":[did]}), StatusCode::BAD_REQUEST).await;
    assert!(!refused["message"].as_str().unwrap().contains("group_carol"));
    diesel::sql_query("DELETE FROM account_blocks")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let added = social(&app, &a, json!({"action":"add_group_members","conversation_id":group,"revision":revision,"members":[did]}), StatusCode::OK).await;
    let revision = added["group"]["revision"].as_i64().unwrap();
    // New members see the retained history, including files.
    assert_eq!(
        kinds(&history(&app, &d, &group, StatusCode::OK).await),
        ["created", "message", "message", "renamed", "added"]
    );
    assert_eq!(download(&app, &d, &file["id"]).await, StatusCode::OK);
    // Group search covers the group for members only.
    let (status, found) = call(&app, SEARCH_PATH, &d, json!({"action":"search","filters":{"query":"hello","scope":{"kind":"private","conversation_id":group}}})).await;
    assert_eq!(status, StatusCode::OK, "{found}");
    assert_eq!(found["messages"].as_array().unwrap().len(), 1);
    assert_eq!(call(&app, SEARCH_PATH, &out, json!({"action":"search","filters":{"query":"hello","scope":{"kind":"private","conversation_id":group}}})).await.0, StatusCode::FORBIDDEN);

    // Removal revokes the removed member's socket, history, files, search and reports.
    super::friends::event(&mut carol, json!({"type":"select","conversation_id":group})).await;
    snapshot(&mut carol, |s| s["conversation_id"] == group).await;
    social(&app, &a, json!({"action":"remove_group_member","conversation_id":group,"revision":revision,"account_id":cid}), StatusCode::OK).await;
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match carol.next().await {
                Some(Ok(WsMessage::Text(text))) if text.contains("\"revoked\"") => break,
                Some(Ok(_)) => {}
                _ => break,
            }
        }
    })
    .await
    .expect("removal must revoke the removed member's friends socket");
    history(&app, &c, &group, StatusCode::FORBIDDEN).await;
    assert_eq!(download(&app, &c, &file["id"]).await, StatusCode::FORBIDDEN);
    assert_eq!(call(&app, SEARCH_PATH, &c, json!({"action":"search","filters":{"query":"hello","scope":{"kind":"private","conversation_id":group}}})).await.0, StatusCode::FORBIDDEN);
    let report = |reporter: &str| {
        let app = app.clone();
        let reporter = reporter.to_owned();
        let group = group.clone();
        let message = with_file["message"]["id"].clone();
        async move {
            call(&app, SAFETY_PATH, &reporter, json!({"action":"create_report","id":Uuid::new_v4(),"target":{"kind":"direct_message","conversation_id":group,"message_id":message},"reason":"test"})).await.0
        }
    };
    assert_eq!(report(&c).await, StatusCode::FORBIDDEN);
    assert_eq!(report(&d).await, StatusCode::OK);
    let mut carol = socket(address, &c).await;
    assert!(snapshot(&mut carol, |_| true).await.get("groups").is_none());

    // Ownership transfer, then the owner leaving hands it to the longest-standing member.
    let current = social(&app, &a, json!({"action":"rename_group","conversation_id":group,"revision":revision + 1,"name":"Weekend"}), StatusCode::OK).await;
    let revision = current["group"]["revision"].as_i64().unwrap();
    social(&app, &a, json!({"action":"transfer_group","conversation_id":group,"revision":revision,"account_id":bid}), StatusCode::OK).await;
    social(&app, &a, json!({"action":"remove_group_member","conversation_id":group,"revision":revision + 1,"account_id":did}), StatusCode::FORBIDDEN).await;
    social(
        &app,
        &b,
        json!({"action":"leave_group","conversation_id":group}),
        StatusCode::OK,
    )
    .await;
    let after = history(&app, &a, &group, StatusCode::OK).await;
    let tail = kinds(&after);
    assert_eq!(
        &tail[tail.len() - 3..],
        ["owner_changed", "left", "owner_changed"]
    );
    assert_eq!(after.last().unwrap()["system"]["person"]["id"], json!(aid));
    history(&app, &b, &group, StatusCode::FORBIDDEN).await;
    // Unfriending the adder does not remove anyone from the group.
    diesel::sql_query("UPDATE friendships SET state='removed'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    history(&app, &d, &group, StatusCode::OK).await;

    // Leaving as the last member deletes the group with its messages and files.
    social(
        &app,
        &d,
        json!({"action":"leave_group","conversation_id":group}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &a,
        json!({"action":"leave_group","conversation_id":group}),
        StatusCode::OK,
    )
    .await;
    let remaining: Vec<Value> = diesel::sql_query(
        "SELECT count(*)::text AS data FROM direct_messages WHERE conversation_id=$1::uuid",
    )
    .bind::<Text, _>(group.as_str().unwrap())
    .load::<Row>(&mut db.pool.get().unwrap())
    .unwrap()
    .into_iter()
    .map(|r| json!(r.data))
    .collect();
    assert_eq!(remaining, [json!("0")]);
    server.abort();
}

#[derive(QueryableByName)]
struct Row {
    #[diesel(sql_type = Text)]
    data: String,
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn group_capacity_blocks_and_account_deletion() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "cap_owner");
    let mut friends = vec![];
    for n in 0..8 {
        let (id, token) = user(&db, &format!("cap_friend_{n}"));
        befriend(&db, aid, id);
        friends.push((id, token));
    }
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    let ids: Vec<AccountId> = friends.iter().map(|f| f.0).collect();
    // Eight members including the creator.
    social(
        &app,
        &a,
        json!({"action":"create_group","client_id":Uuid::new_v4(),"members":ids}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let created = social(
        &app,
        &a,
        json!({"action":"create_group","client_id":Uuid::new_v4(),"members":ids[..7]}),
        StatusCode::OK,
    )
    .await;
    let group = created["group"]["conversation_id"].clone();
    let revision = created["group"]["revision"].clone();
    social(&app, &a, json!({"action":"add_group_members","conversation_id":group,"revision":revision,"members":[ids[7]]}), StatusCode::BAD_REQUEST).await;

    // A block between members removes nobody but hides unread counts and rings.
    let (bid, b) = (friends[0].0, friends[0].1.clone());
    let (cid, c) = (friends[1].0, friends[1].1.clone());
    block(&db, bid, cid);
    let mut bob = socket(address, &b).await;
    snapshot(&mut bob, |s| s["groups"][0]["unread_count"] == 0).await;
    social(&app, &c, json!({"action":"send","conversation_id":group,"client_id":Uuid::new_v4(),"content":"blocked author"}), StatusCode::OK).await;
    social(&app, &a, json!({"action":"send","conversation_id":group,"client_id":Uuid::new_v4(),"content":"owner"}), StatusCode::OK).await;
    let seen = snapshot(&mut bob, |s| s["groups"][0]["unread_count"] == 1).await;
    // Presence of the blocked member is hidden even while connected.
    let _carol = socket(address, &c).await;
    tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
    let members = seen["groups"][0]["members"].as_array().unwrap();
    let carol_member = members
        .iter()
        .find(|m| m["person"]["id"] == json!(cid))
        .unwrap();
    assert_eq!(carol_member["online"], false);
    let started = social(
        &app,
        &c,
        json!({"action":"start_call","conversation_id":group}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(started["call"]["participants"], json!([cid]));
    let ringing = snapshot(&mut bob, |s| {
        s["group_calls"].as_array().is_some_and(|c| !c.is_empty())
    })
    .await;
    assert_eq!(ringing["group_calls"][0]["ringing"], false);
    // Blocked members can still join an ongoing call explicitly.
    let joined = social(
        &app,
        &b,
        json!({"action":"accept_call","call_id":started["call"]["id"]}),
        StatusCode::OK,
    )
    .await;
    let mut participants = vec![bid, cid];
    participants.sort();
    assert_eq!(joined["call"]["participants"], json!(participants));

    // Deleting an account removes its membership and authored messages; an
    // owner's deletion passes ownership on.
    let (status, body) = call(
        &app,
        thiscord_shared::account::ACCOUNT_PATH,
        &a,
        json!({"action":"delete_account","confirmation":"cap_owner"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let messages = history(&app, &b, &group, StatusCode::OK).await;
    assert!(messages.iter().all(|m| m["author_id"] != json!(aid)));
    let after = snapshot(&mut bob, |s| {
        s["groups"][0]["members"]
            .as_array()
            .is_some_and(|m| m.len() == 7)
    })
    .await;
    assert_eq!(after["groups"][0]["members"][0]["owner"], true);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL"]
async fn group_call_lifecycle_rings_joins_and_ends_with_the_last_member() {
    let _lock = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "gcall_a");
    let (bid, b) = user(&db, "gcall_b");
    let (cid, c) = user(&db, "gcall_c");
    let (_, out) = user(&db, "gcall_out");
    befriend(&db, aid, bid);
    befriend(&db, aid, cid);
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    let created = social(
        &app,
        &a,
        json!({"action":"create_group","client_id":Uuid::new_v4(),"members":[bid,cid]}),
        StatusCode::OK,
    )
    .await;
    let group = created["group"]["conversation_id"].clone();
    let mut carol = socket(address, &c).await;
    snapshot(&mut carol, |s| {
        s["groups"].as_array().is_some_and(|g| g.len() == 1)
    })
    .await;

    social(
        &app,
        &out,
        json!({"action":"start_call","conversation_id":group}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let started = social(
        &app,
        &a,
        json!({"action":"start_call","conversation_id":group}),
        StatusCode::OK,
    )
    .await;
    let id = started["call"]["id"].clone();
    assert_eq!(started["call"]["participants"], json!([aid]));
    let rung = snapshot(&mut carol, |s| s["group_calls"][0]["ringing"] == true).await;
    assert_eq!(rung["group_calls"][0]["started_by"], json!(aid));
    social(
        &app,
        &out,
        json!({"action":"accept_call","call_id":id}),
        StatusCode::FORBIDDEN,
    )
    .await;
    // Starting again while live joins the same call.
    let joined = social(
        &app,
        &b,
        json!({"action":"start_call","conversation_id":group}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(joined["call"]["id"], id);
    // Declining stops the ring without ending the call.
    social(
        &app,
        &c,
        json!({"action":"end_call","call_id":id}),
        StatusCode::OK,
    )
    .await;
    let declined = snapshot(&mut carol, |s| s["group_calls"][0]["ringing"] == false).await;
    assert_eq!(
        declined["group_calls"][0]["participants"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    // Leaving keeps it running for the rest; the last one out ends it.
    social(
        &app,
        &a,
        json!({"action":"end_call","call_id":id}),
        StatusCode::OK,
    )
    .await;
    snapshot(&mut carol, |s| {
        s["group_calls"][0]["participants"] == json!([bid])
    })
    .await;
    social(
        &app,
        &c,
        json!({"action":"accept_call","call_id":id}),
        StatusCode::OK,
    )
    .await;
    let mut both = vec![bid, cid];
    both.sort();
    snapshot(&mut carol, |s| {
        s["group_calls"][0]["participants"] == json!(both)
    })
    .await;
    // Removing a joined member drops them from the call immediately.
    let revision = social(&app, &a, json!({"action":"rename_group","conversation_id":group,"revision":created["group"]["revision"],"name":"Call"}), StatusCode::OK).await["group"]["revision"].clone();
    social(&app, &a, json!({"action":"remove_group_member","conversation_id":group,"revision":revision,"account_id":cid}), StatusCode::OK).await;
    social(
        &app,
        &c,
        json!({"action":"end_call","call_id":id}),
        StatusCode::FORBIDDEN,
    )
    .await;
    social(
        &app,
        &b,
        json!({"action":"end_call","call_id":id}),
        StatusCode::OK,
    )
    .await;
    let mut alice = socket(address, &a).await;
    snapshot(&mut alice, |s| s.get("group_calls").is_none()).await;
    // A new call gets a new ID.
    let next = social(
        &app,
        &a,
        json!({"action":"start_call","conversation_id":group}),
        StatusCode::OK,
    )
    .await;
    assert_ne!(next["call"]["id"], id);
    server.abort();
}
