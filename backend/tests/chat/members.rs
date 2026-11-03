use super::friends::{snapshot, social, socket as friends_socket};
use super::*;
use thiscord_shared::{account::ACCOUNT_PATH, safety::SAFETY_PATH};

async fn account(app: &Router, token: &str, value: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, ACCOUNT_PATH, token, value).await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn set_status(app: &Router, token: &str, status: Value, expected: StatusCode) -> Value {
    account(
        app,
        token,
        json!({"action":"set_custom_status","status":status}),
        expected,
    )
    .await
}
async fn profile(app: &Router, token: &str, guild: &Value, id: AccountId) -> (StatusCode, Value) {
    call(
        app,
        CHAT_PATH,
        token,
        json!({"action":"member_profile","guild_id":guild,"account_id":id}),
    )
    .await
}
async fn block(app: &Router, token: &str, id: AccountId, blocked: bool) {
    let action = if blocked { "block" } else { "unblock" };
    let (status, body) = call(
        app,
        SAFETY_PATH,
        token,
        json!({"action":action,"account_id":id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
fn member(list: &Value, id: AccountId) -> Option<Value> {
    list["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["account_id"] == json!(id))
        .cloned()
}
async fn subscribe_members(socket: &mut Socket, serial: u64, guild: &Value, channel: &Value) {
    socket.send(WsMessage::Text(json!({"version":1,"event":{"type":"subscribe","subscription":serial,"guild_id":guild,"channel_id":channel,"members":true}}).to_string().into())).await.unwrap();
    assert_eq!(event(socket, "subscribed").await["subscription"], serial);
}
/// Commit notifications and presence ticks must deliver well before the
/// one-minute safety refresh.
async fn update_where(
    socket: &mut Socket,
    serial: u64,
    kind: &str,
    predicate: impl Fn(&Value) -> bool,
) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            let update = pushed(socket, serial, kind).await;
            if predicate(&update) {
                return update;
            }
        }
    })
    .await
    .expect("expected a pushed member list update")
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn custom_status_validates_expires_and_respects_guild_audience_and_blocks() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, state, _) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let owner_id: AccountId = serde_json::from_value(state["guild"]["owner"].clone()).unwrap();
    let (outsider_id, outsider) = user(&db, "outsider");

    // Trimmed, stored and returned as plain text; no expiry means "don't clear".
    let set = set_status(
        &app,
        &guest,
        json!({"emoji":"🎮","text":"  Raiding <b>now</b>  "}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        set["account"]["custom_status"],
        json!({"emoji":"🎮","text":"Raiding <b>now</b>"})
    );
    let current = account(&app, &guest, json!({"action":"current"}), StatusCode::OK).await;
    assert_eq!(
        current["account"]["custom_status"],
        set["account"]["custom_status"]
    );
    for invalid in [
        json!({"text":"x".repeat(129)}),
        json!({"text":"two\nlines"}),
        json!({"emoji":"x","text":"palette only"}),
        json!({"emoji":"🎮🎮"}),
        json!({"text":"   "}),
        json!({"text":"never","expires_in":0}),
        json!({"text":"too long","expires_in":604_801}),
        json!({"text":"forged","expires_at":"2030-01-01T00:00:00Z"}),
    ] {
        set_status(&app, &guest, invalid, StatusCode::BAD_REQUEST).await;
    }
    // Rejected updates keep the previous status.
    let current = account(&app, &guest, json!({"action":"current"}), StatusCode::OK).await;
    assert_eq!(
        current["account"]["custom_status"]["text"],
        "Raiding <b>now</b>"
    );
    set_status(&app, "", json!({"text":"anon"}), StatusCode::UNAUTHORIZED).await;

    // Fellow members read the public profile; outsiders and non-members cannot.
    diesel::sql_query("UPDATE accounts SET bio='Hello there' WHERE id=$1::uuid")
        .bind::<Text, _>(guest_id.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let (status, body) = profile(&app, &owner, &guild, guest_id).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let card = &body["profile"];
    assert_eq!(card["username"], "guest");
    assert_eq!(card["bio"], "Hello there");
    assert_eq!(card["custom_status"]["emoji"], "🎮");
    assert!(card.get("email").is_none() && card.get("owner").is_none());
    let (_, body) = profile(&app, &guest, &guild, owner_id).await;
    assert_eq!(body["profile"]["owner"], true);
    assert_eq!(
        profile(&app, &outsider, &guild, guest_id).await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        profile(&app, &owner, &guild, outsider_id).await.0,
        StatusCode::NOT_FOUND
    );

    // A block in either direction withholds the status, but not the public profile.
    set_status(&app, &owner, json!({"text":"Owner status"}), StatusCode::OK).await;
    block(&app, &owner, guest_id, true).await;
    for (viewer, target) in [(&owner, guest_id), (&guest, owner_id)] {
        let (status, body) = profile(&app, viewer, &guild, target).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["profile"].get("custom_status").is_none(), "{body}");
        assert!(body["profile"]["username"].is_string());
    }
    block(&app, &owner, guest_id, false).await;
    let (_, body) = profile(&app, &guest, &guild, owner_id).await;
    assert_eq!(body["profile"]["custom_status"]["text"], "Owner status");

    // Expiry is computed by the server and expired statuses read as cleared.
    let set = set_status(
        &app,
        &guest,
        json!({"text":"Brb","expires_in":3600}),
        StatusCode::OK,
    )
    .await;
    let expires: thiscord_shared::Timestamp =
        serde_json::from_value(set["account"]["custom_status"]["expires_at"].clone()).unwrap();
    let remaining = expires - chrono::Utc::now();
    assert!(remaining > chrono::Duration::minutes(59) && remaining <= chrono::Duration::hours(1));
    assert!(set["account"]["custom_status"].get("emoji").is_none());
    diesel::sql_query(
        "UPDATE account_custom_statuses SET expires_at=now()-interval '1 second' WHERE account_id=$1::uuid",
    )
    .bind::<Text, _>(guest_id.to_string())
    .execute(&mut db.pool.get().unwrap())
    .unwrap();
    let current = account(&app, &guest, json!({"action":"current"}), StatusCode::OK).await;
    assert!(current["account"].get("custom_status").is_none());
    let (_, body) = profile(&app, &owner, &guild, guest_id).await;
    assert!(body["profile"].get("custom_status").is_none());

    // Clearing deletes the row; account deletion cascades it.
    set_status(&app, &guest, json!({"emoji":"✅"}), StatusCode::OK).await;
    let cleared = set_status(&app, &guest, Value::Null, StatusCode::OK).await;
    assert!(cleared["account"].get("custom_status").is_none());
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        count: i64,
    }
    let rows = diesel::sql_query(
        "SELECT count(*) AS count FROM account_custom_statuses WHERE account_id=$1::uuid",
    )
    .bind::<Text, _>(guest_id.to_string())
    .get_result::<Count>(&mut db.pool.get().unwrap())
    .unwrap()
    .count;
    assert_eq!(rows, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn member_list_pushes_presence_status_blocks_and_removal_and_syncs_devices() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let owner_id: AccountId = serde_json::from_value(state["guild"]["owner"].clone()).unwrap();
    let (outsider_id, outsider) = user(&db, "outsider");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });

    // Opt-in only: a legacy subscription never receives member list events.
    let mut legacy = session_socket(address, &owner).await;
    event(&mut legacy, "authenticated").await;
    subscribe(&mut legacy, 1, guild.clone(), channel.clone()).await;

    let mut viewer = session_socket(address, &owner).await;
    event(&mut viewer, "authenticated").await;
    subscribe_members(&mut viewer, 1, &guild, &channel).await;
    let list = pushed(&mut viewer, 1, "members").await;
    let names: Vec<_> = list["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["username"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["guest", "owner"]); // Sorted by display name.
    let own = member(&list, owner_id).unwrap();
    assert_eq!(
        (own["owner"].clone(), own["online"].clone()),
        (json!(true), json!(false))
    );
    assert!(member(&list, outsider_id).is_none());
    for entry in list["members"].as_array().unwrap() {
        assert!(entry.get("email").is_none() && entry.get("bio").is_none());
    }

    // Account-wide presence (an authenticated friends socket) is pushed live.
    let mut guest_device = friends_socket(address, &guest).await;
    snapshot(&mut guest_device, |_| true).await;
    let presence = update_where(&mut viewer, 1, "member_presence", |u| {
        u["online"].as_array().unwrap().contains(&json!(guest_id))
    })
    .await;
    assert!(
        !presence["online"]
            .as_array()
            .unwrap()
            .contains(&json!(owner_id))
    );

    // Setting a status updates every fellow member's list and the guest's other device.
    let second = extra_session(&db, guest_id);
    let mut other_device = friends_socket(address, &second).await;
    snapshot(&mut other_device, |s| s.get("own_status").is_none()).await;
    set_status(
        &app,
        &guest,
        json!({"emoji":"🎮","text":"Away"}),
        StatusCode::OK,
    )
    .await;
    let list = update_where(&mut viewer, 1, "members", |u| {
        member(u, guest_id).is_some_and(|m| m["custom_status"]["text"] == "Away")
    })
    .await;
    assert_eq!(member(&list, guest_id).unwrap()["online"], true);
    let synced = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        snapshot(&mut other_device, |s| s["own_status"]["text"] == "Away"),
    )
    .await
    .expect("status must reach the account's other devices before the periodic refresh");
    assert_eq!(synced["own_status"]["emoji"], "🎮");

    // Friends see the status only once the request is accepted.
    let mut outsider_device = friends_socket(address, &outsider).await;
    snapshot(&mut outsider_device, |_| true).await;
    social(
        &app,
        &outsider,
        json!({"action":"add","account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    let pending = snapshot(&mut outsider_device, |s| {
        s["friends"][0]["relationship"] == "outgoing"
    })
    .await;
    assert!(
        pending["friends"][0]["person"]
            .get("custom_status")
            .is_none()
    );
    social(
        &app,
        &guest,
        json!({"action":"accept","account_id":outsider_id}),
        StatusCode::OK,
    )
    .await;
    snapshot(&mut outsider_device, |s| {
        s["friends"][0]["person"]["custom_status"]["text"] == "Away"
    })
    .await;

    // Clearing propagates to devices and the member list.
    set_status(&app, &guest, Value::Null, StatusCode::OK).await;
    tokio::time::timeout(
        std::time::Duration::from_secs(3),
        snapshot(&mut other_device, |s| s.get("own_status").is_none()),
    )
    .await
    .expect("cleared status must sync");
    update_where(&mut viewer, 1, "members", |u| {
        member(u, guest_id).is_some_and(|m| m.get("custom_status").is_none())
    })
    .await;

    // The legacy subscription saw none of this.
    connected(&mut legacy).await;
    tokio::time::timeout(std::time::Duration::from_millis(500), async {
        while let Some(Ok(message)) = legacy.next().await {
            if let WsMessage::Text(text) = message {
                let value: Value = serde_json::from_str(&text).unwrap();
                let kind = value["event"]["event"]["type"].as_str().unwrap_or_default();
                assert!(kind != "members" && kind != "member_presence", "{value}");
            }
        }
    })
    .await
    .ok();

    // Across a block, a fresh list shows neither presence nor status. Blocking
    // revokes both accounts' existing sockets; reconnecting clients resubscribe.
    set_status(&app, &guest, json!({"text":"Hidden"}), StatusCode::OK).await;
    block(&app, &guest, owner_id, true).await;
    let mut blocked_view = session_socket(address, &owner).await;
    event(&mut blocked_view, "authenticated").await;
    subscribe_members(&mut blocked_view, 2, &guild, &channel).await;
    let list = pushed(&mut blocked_view, 2, "members").await;
    let hidden = member(&list, guest_id).expect("membership itself stays visible");
    assert_eq!(hidden["online"], false);
    assert!(hidden.get("custom_status").is_none());
    block(&app, &guest, owner_id, false).await;
    let mut view = session_socket(address, &owner).await;
    event(&mut view, "authenticated").await;
    subscribe_members(&mut view, 3, &guild, &channel).await;
    let list = pushed(&mut view, 3, "members").await;
    assert_eq!(
        member(&list, guest_id).unwrap()["custom_status"]["text"],
        "Hidden"
    );

    // Removing a member refreshes lists without revoking other viewers.
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member","account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    update_where(&mut view, 3, "members", |u| member(u, guest_id).is_none()).await;
    server.abort();
}
