//! Role colours, "show separately" and per-server nicknames.
use super::*;

/// A request from a current client, which declares its permission set.
async fn current(app: &Router, token: &str, command: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, &permissions_path(), token, command).await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn inspect(app: &Router, token: &str, guild: &Value) -> Value {
    current(
        app,
        token,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await["state"]
        .clone()
}
fn role<'a>(state: &'a Value, name: &str) -> &'a Value {
    state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == name)
        .unwrap()
}
fn row(state: &Value, id: AccountId) -> &Value {
    state["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["account_id"] == json!(id))
        .unwrap()
}
async fn nickname(
    app: &Router,
    token: &str,
    state: &Value,
    target: AccountId,
    nickname: Value,
    expected: StatusCode,
) -> Value {
    current(
        app,
        token,
        json!({"action":"change","guild_id":state["guild"]["id"],"revision":state["guild"]["revision"],
            "change":{"action":"set_nickname","account_id":target,"nickname":nickname}}),
        expected,
    )
    .await
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn role_display_settings_follow_hierarchy_and_survive_older_clients() {
    let db = database();
    let (_, owner) = user(&db, "display_owner");
    let (manager, manager_token) = user(&db, "display_manager");
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Display"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"add_member","username":"display_manager"}),
        StatusCode::OK,
    )
    .await;
    // New roles default to no colour and no separate section; a released
    // client's create_role carries no appearance at all.
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_role","name":"Plain","position":2,"permissions":[]}),
        StatusCode::OK,
    )
    .await;
    assert!(role(&state, "Plain").get("color").is_none());
    assert!(role(&state, "Plain").get("hoist").is_none());
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_role","name":"Managers","position":5,"permissions":["manage_roles"],
        "appearance":{"color":0x2fa85a,"hoist":true}}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_role","name":"Seniors","position":7,"permissions":[],
        "appearance":{"color":0xf05d4f,"hoist":true}}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(role(&state, "Managers")["color"], 0x2fa85a);
    assert_eq!(role(&state, "Managers")["hoist"], true);
    let managers = role(&state, "Managers")["id"].clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"assign_role","account_id":manager,"role_id":managers,"assigned":true}),
        StatusCode::OK,
    )
    .await;

    // Recolouring follows the role hierarchy: below the actor's highest role only.
    let mut seen = inspect(&app, &manager_token, &guild).await;
    let plain = role(&seen, "Plain")["id"].clone();
    change(
        &app,
        &manager_token,
        &mut seen,
        json!({"action":"edit_role","role_id":plain,"name":"Plain","position":2,"permissions":[],
        "appearance":{"color":0x2582eb,"hoist":true}}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(role(&seen, "Plain")["color"], 0x2582eb);
    for (name, position) in [("Seniors", 7), ("Managers", 5)] {
        let id = role(&seen, name)["id"].clone();
        let permissions = role(&seen, name)["permissions"].clone();
        change(
            &app,
            &manager_token,
            &mut seen,
            json!({"action":"edit_role","role_id":id,"name":name,"position":position,
            "permissions":permissions,"appearance":{"color":null,"hoist":false}}),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    // Everyone can never be shown separately.
    state = inspect(&app, &owner, &guild).await;
    let everyone = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["everyone"] == true)
        .unwrap()
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"edit_role","role_id":everyone["id"],"name":everyone["name"],"position":0,
        "permissions":everyone["permissions"],"appearance":{"color":0x123456,"hoist":true}}),
        StatusCode::FORBIDDEN,
    )
    .await;
    // Invalid colours fail deserialization.
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_role","name":"Bad","position":1,"permissions":[],
        "appearance":{"color":0x1000000,"hoist":false}}),
        StatusCode::BAD_REQUEST,
    )
    .await;

    // A stale revision is rejected without applying the edit.
    let stale = seen["guild"]["revision"].as_i64().unwrap() - 1;
    let (status, _) = call(&app, &permissions_path(), &manager_token, json!({"action":"change","guild_id":guild,"revision":stale,
        "change":{"action":"edit_role","role_id":plain,"name":"Plain","position":2,"permissions":[],"appearance":{"color":null,"hoist":false}}})).await;
    assert_eq!(status, StatusCode::CONFLICT);

    // A released client (no permission set, no appearance) edits roles without
    // seeing or erasing colours, sections or newer permission names.
    let legacy = command(
        &app,
        &owner,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let legacy_everyone = legacy["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["everyone"] == true)
        .unwrap()
        .clone();
    assert!(
        !legacy_everyone["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("change_nickname"))
    );
    let mut legacy_state = legacy.clone();
    change(
        &app,
        &owner,
        &mut legacy_state,
        json!({"action":"edit_role","role_id":legacy_everyone["id"],"name":"everyone",
        "position":0,"permissions":legacy_everyone["permissions"]}),
        StatusCode::OK,
    )
    .await;
    change(&app,&owner,&mut legacy_state,json!({"action":"edit_role","role_id":plain,"name":"Renamed","position":2,"permissions":[]}),StatusCode::OK).await;
    let after = inspect(&app, &owner, &guild).await;
    assert_eq!(role(&after, "Renamed")["color"], 0x2582eb);
    assert_eq!(role(&after, "Renamed")["hoist"], true);
    let everyone = after["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["everyone"] == true)
        .unwrap();
    assert!(
        everyone["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("change_nickname"))
    );
    // Released clients' previews and moderation views never contain new names.
    let preview = command(
        &app,
        &owner,
        json!({"action":"preview","guild_id":guild,"account_id":manager,"channel_id":null}),
        StatusCode::OK,
    )
    .await;
    assert!(
        !preview["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("change_nickname"))
    );
    let preview = current(
        &app,
        &owner,
        json!({"action":"preview","guild_id":guild,"account_id":manager,"channel_id":null}),
        StatusCode::OK,
    )
    .await;
    assert!(
        preview["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("change_nickname"))
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn nicknames_are_scoped_validated_hierarchical_and_audited() {
    let db = database();
    let (owner, owner_token) = user(&db, "nick_owner");
    let (moderator, moderator_token) = user(&db, "nick_moderator");
    let (member, member_token) = user(&db, "nick_member");
    let (peer, peer_token) = user(&db, "nick_peer");
    permissions::bootstrap_owner(&db.pool, "nick_owner").unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &owner_token,
        json!({"action":"create_guild","name":"Nicks"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    let mut other = command(
        &app,
        &owner_token,
        json!({"action":"create_guild","name":"Elsewhere"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    for username in ["nick_moderator", "nick_member", "nick_peer"] {
        change(
            &app,
            &owner_token,
            &mut state,
            json!({"action":"add_member","username":username}),
            StatusCode::OK,
        )
        .await;
    }
    change(
        &app,
        &owner_token,
        &mut other,
        json!({"action":"add_member","username":"nick_member"}),
        StatusCode::OK,
    )
    .await;
    change(&app,&owner_token,&mut state,json!({"action":"create_role","name":"Mods","position":5,"permissions":["manage_nicknames"]}),StatusCode::OK).await;
    let mods = role(&state, "Mods")["id"].clone();
    change(
        &app,
        &owner_token,
        &mut state,
        json!({"action":"assign_role","account_id":moderator,"role_id":mods,"assigned":true}),
        StatusCode::OK,
    )
    .await;

    // Members change their own nickname by default; the guild-scoped name is
    // used in member data while the profile keeps the account name.
    let home = current(
        &app,
        &member_token,
        json!({"action":"view_guild","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(home["home"]["can_change_nickname"], true);
    let mut seen = inspect(&app, &owner_token, &guild).await;
    assert_eq!(
        nickname(
            &app,
            &member_token,
            &seen,
            member,
            json!("  Kanka  "),
            StatusCode::OK
        )
        .await["result"],
        "done"
    );
    seen = inspect(&app, &owner_token, &guild).await;
    assert_eq!(row(&seen, member)["display_name"], "Kanka");
    assert_eq!(row(&seen, member)["nickname"], "Kanka");
    // Scoped to this server: the other server still shows the profile name.
    let elsewhere = inspect(&app, &owner_token, &other["guild"]["id"]).await;
    assert_eq!(row(&elsewhere, member)["display_name"], "nick_member");
    assert!(row(&elsewhere, member).get("nickname").is_none());
    let (status, profile) = call(
        &app,
        thiscord_shared::chat::CHAT_PATH,
        &peer_token,
        json!({"action":"member_profile","guild_id":guild,"account_id":member}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{profile}");
    assert_eq!(profile["profile"]["display_name"], "nick_member");
    assert_eq!(profile["profile"]["nickname"], "Kanka");

    // Validation matches display names; the stored name is unchanged.
    for bad in [
        json!(""),
        json!("   "),
        json!("a\u{7}b"),
        json!("x".repeat(65)),
    ] {
        nickname(
            &app,
            &member_token,
            &seen,
            member,
            bad,
            StatusCode::BAD_REQUEST,
        )
        .await;
    }
    // Stale revisions are rejected.
    let mut stale = seen.clone();
    stale["guild"]["revision"] = json!(seen["guild"]["revision"].as_i64().unwrap() - 1);
    nickname(
        &app,
        &member_token,
        &stale,
        member,
        json!("Late"),
        StatusCode::CONFLICT,
    )
    .await;

    // Without ManageNicknames nobody renames anyone else, even lower ranks.
    nickname(
        &app,
        &member_token,
        &seen,
        peer,
        json!("Renamed"),
        StatusCode::FORBIDDEN,
    )
    .await;
    // Moderators rename strictly lower-ranked members only, never the owner,
    // and every moderator rename is audited.
    nickname(
        &app,
        &moderator_token,
        &seen,
        owner,
        json!("Boss"),
        StatusCode::FORBIDDEN,
    )
    .await;
    nickname(
        &app,
        &moderator_token,
        &seen,
        member,
        json!("Calmer"),
        StatusCode::OK,
    )
    .await;
    seen = inspect(&app, &owner_token, &guild).await;
    nickname(
        &app,
        &moderator_token,
        &seen,
        member,
        Value::Null,
        StatusCode::OK,
    )
    .await;
    seen = inspect(&app, &owner_token, &guild).await;
    assert_eq!(row(&seen, member)["display_name"], "nick_member");
    assert!(row(&seen, member).get("nickname").is_none());
    change(
        &app,
        &owner_token,
        &mut seen,
        json!({"action":"create_role","name":"Peers","position":5,"permissions":[]}),
        StatusCode::OK,
    )
    .await;
    let peers = role(&seen, "Peers")["id"].clone();
    change(
        &app,
        &owner_token,
        &mut seen,
        json!({"action":"assign_role","account_id":peer,"role_id":peers,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    nickname(
        &app,
        &moderator_token,
        &seen,
        peer,
        json!("Equal"),
        StatusCode::FORBIDDEN,
    )
    .await;
    // Self-renames are not moderation and are not audited.
    nickname(
        &app,
        &moderator_token,
        &seen,
        moderator,
        json!("Mod"),
        StatusCode::OK,
    )
    .await;
    seen = inspect(&app, &owner_token, &guild).await;
    let log = command(
        &app,
        &owner_token,
        json!({"action":"audit_log","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    let entries: Vec<_> = log["page"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["action"] == "set_nickname")
        .collect();
    assert_eq!(entries.len(), 2, "{log}");
    let details: Vec<Value> = entries
        .iter()
        .map(|e| serde_json::from_str(e["details"].as_str().unwrap()).unwrap())
        .collect();
    assert!(
        entries
            .iter()
            .all(|e| e["actor"] == json!(moderator) && e["target"] == json!(member))
    );
    assert!(
        details.contains(&json!({"nickname":"Calmer","previous":"Kanka"})),
        "{details:?}"
    );
    assert!(
        details.contains(&json!({"nickname":null,"previous":"Calmer"})),
        "{details:?}"
    );

    // Removing ChangeNickname from Everyone stops self-service; owners keep it.
    let everyone = seen["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["everyone"] == true)
        .unwrap()
        .clone();
    let reduced: Vec<Value> = everyone["permissions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| **p != "change_nickname")
        .cloned()
        .collect();
    let revision = seen["guild"]["revision"].clone();
    seen = current(&app, &owner_token, json!({"action":"change","guild_id":guild,"revision":revision,"change":{"action":"edit_role",
        "role_id":everyone["id"],"name":"everyone","position":0,"permissions":reduced}}), StatusCode::OK).await["state"].clone();
    nickname(
        &app,
        &member_token,
        &seen,
        member,
        json!("Nope"),
        StatusCode::FORBIDDEN,
    )
    .await;
    let home = current(
        &app,
        &member_token,
        json!({"action":"view_guild","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert!(home["home"].get("can_change_nickname").is_none());
    nickname(
        &app,
        &owner_token,
        &seen,
        owner,
        json!("Owner"),
        StatusCode::OK,
    )
    .await;

    // Leaving drops the nickname with the membership.
    seen = inspect(&app, &owner_token, &guild).await;
    nickname(
        &app,
        &owner_token,
        &seen,
        member,
        json!("Temporary"),
        StatusCode::OK,
    )
    .await;
    seen = inspect(&app, &owner_token, &guild).await;
    change(
        &app,
        &member_token,
        &mut seen.clone(),
        json!({"action":"leave"}),
        StatusCode::OK,
    )
    .await;
    seen = inspect(&app, &owner_token, &guild).await;
    change(
        &app,
        &owner_token,
        &mut seen,
        json!({"action":"add_member","username":"nick_member"}),
        StatusCode::OK,
    )
    .await;
    seen = inspect(&app, &owner_token, &guild).await;
    assert!(row(&seen, member).get("nickname").is_none());
}
