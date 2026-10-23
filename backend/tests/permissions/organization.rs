use super::*;

async fn snapshot(app: &Router, token: &str, guild: &Value) -> Value {
    command(
        app,
        token,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await["state"]
        .clone()
}
async fn invite(app: &Router, token: &str, state: &mut Value, uses: u32) -> Value {
    let response=command(app,token,json!({"action":"create_invite","guild_id":state["guild"]["id"],"revision":state["guild"]["revision"],"duration_seconds":3600,"max_uses":uses}),StatusCode::OK).await;
    state["guild"] = response["guild"].clone();
    response
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn invites_atomic_use_quota_verification_bans_and_timeout_retention() {
    let db = database();
    let (_, ot) = user(&db, "invite_owner");
    let (a, at) = user(&db, "invite_a");
    let (b, bt) = user(&db, "invite_b");
    let (unverified, ut) = user(&db, "invite_unverified");
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Invites","password":"private"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    let first = invite(&app, &ot, &mut state, 1).await;
    assert_eq!(first["code"].as_str().unwrap().len(), 43);
    let listed = command(
        &app,
        &ot,
        json!({"action":"list_invites","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert!(!listed.to_string().contains(first["code"].as_str().unwrap()));
    assert!(!listed.to_string().contains("token_hash"));
    command(
        &app,
        &at,
        json!({"action":"list_invites","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    diesel::sql_query("UPDATE accounts SET email_verified=FALSE WHERE id=$1::uuid")
        .bind::<Text, _>(unverified.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    command(
        &app,
        &ut,
        json!({"action":"accept_invite","code":first["code"]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let join = json!({"action":"accept_invite","code":first["code"]});
    let (one, two) = tokio::join!(
        call(&app, PERMISSIONS_PATH, &at, join.clone()),
        call(&app, PERMISSIONS_PATH, &bt, join)
    );
    assert_eq!(
        [one.0, two.0]
            .into_iter()
            .filter(StatusCode::is_success)
            .count(),
        1
    );
    assert!([one.0, two.0].contains(&StatusCode::FORBIDDEN));
    let (winner, wt, lt) = if one.0.is_success() {
        (a, &at, &bt)
    } else {
        (b, &bt, &at)
    };
    command(
        &app,
        wt,
        json!({"action":"accept_invite","code":first["code"]}),
        StatusCode::OK,
    )
    .await;
    state = snapshot(&app, &ot, &guild).await;
    let second = invite(&app, &ot, &mut state, 50).await;
    // Owner's current role, not creator's or recipient's quota, governs redemption.
    diesel::sql_query("UPDATE instance_role_limits SET members=2 WHERE builtin AND role='user'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    command(
        &app,
        lt,
        json!({"action":"accept_invite","code":second["code"]}),
        StatusCode::CONFLICT,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"timeout_member","account_id":winner,"duration_seconds":600}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"remove_member","account_id":winner}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        wt,
        json!({"action":"accept_invite","code":second["code"]}),
        StatusCode::OK,
    )
    .await;
    let effective = command(
        &app,
        wt,
        json!({"action":"preview","guild_id":guild,"account_id":winner}),
        StatusCode::OK,
    )
    .await;
    assert!(
        !effective["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("join_voice"))
    );
    state = snapshot(&app, &ot, &guild).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"ban_member","account_id":winner}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        wt,
        json!({"action":"accept_invite","code":second["code"]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(&app,&ot,json!({"action":"revoke_invite","guild_id":guild,"revision":state["guild"]["revision"],"invite_id":second["invite"]["id"]}),StatusCode::OK).await;
    // A fresh recipient avoids the join attempt limiter while exercising revoked/expired codes.
    let (_, ct) = user(&db, "invite_c");
    command(
        &app,
        &ct,
        json!({"action":"accept_invite","code":second["code"]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    state = snapshot(&app, &ot, &guild).await;
    let expired = invite(&app, &ot, &mut state, 1).await;
    diesel::sql_query(
        "UPDATE guild_invites SET expires_at=now()-interval '1 second' WHERE id=$1::uuid",
    )
    .bind::<Text, _>(expired["invite"]["id"].as_str().unwrap())
    .execute(&mut db.pool.get().unwrap())
    .unwrap();
    command(
        &app,
        &ct,
        json!({"action":"accept_invite","code":expired["code"]}),
        StatusCode::FORBIDDEN,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn organization_preserves_identity_permissions_history_and_rejects_foreign_ids() {
    let db = database();
    let (_, ot) = user(&db, "organization_owner");
    let (member, mt) = user(&db, "organization_member");
    let (_, xt) = user(&db, "organization_other");
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Organized"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"organization_member"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"private","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":["view_channel"]}),StatusCode::OK).await;
    let (_,message)=call(&app,thiscord_shared::chat::CHAT_PATH,&ot,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"retained history"})).await;
    assert!(message.get("error").is_none());
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_category","name":"Private rooms"}),
        StatusCode::OK,
    )
    .await;
    let category = state["categories"][0]["id"].clone();
    let stale = state["guild"]["revision"].clone();
    change(&app,&ot,&mut state,json!({"action":"edit_channel","channel_id":channel,"name":"renamed","category_id":category,"position":6}),StatusCode::OK).await;
    command(&app,&ot,json!({"action":"change","guild_id":guild,"revision":stale,"change":{"action":"edit_category","category_id":category,"name":"stale","position":1}}),StatusCode::CONFLICT).await;
    let home = command(
        &app,
        &mt,
        json!({"action":"view_guild","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(home["home"]["channels"], json!([]));
    assert_eq!(home["home"]["categories"], json!([]));
    command(
        &app,
        &mt,
        json!({"action":"inspect_organization","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let mut other = command(
        &app,
        &xt,
        json!({"action":"create_guild","name":"Other"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &xt,
        &mut other,
        json!({"action":"create_category","name":"Foreign"}),
        StatusCode::OK,
    )
    .await;
    change(&app,&ot,&mut state,json!({"action":"edit_channel","channel_id":channel,"name":"wrong","category_id":other["categories"][0]["id"],"position":0}),StatusCode::FORBIDDEN).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"delete_category","category_id":category}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(state["channels"][0]["id"], channel);
    assert_eq!(state["channels"][0]["name"], "renamed");
    assert!(state["channels"][0]["category_id"].is_null());
    assert_eq!(state["overrides"].as_array().unwrap().len(), 1);
    let (status, history) = call(
        &app,
        thiscord_shared::chat::CHAT_PATH,
        &ot,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(history.to_string().contains("retained history"));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn moderation_reason_audit_is_atomic_bounded_and_scoped() {
    let db = database();
    let (owner, ot) = user(&db, "audit_owner");
    let (member, mt) = user(&db, "audit_member");
    let (_, xt) = user(&db, "audit_other");
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Audit"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"audit_member"}),
        StatusCode::OK,
    )
    .await;
    let request = json!({"action":"moderate","guild_id":guild,"revision":state["guild"]["revision"],"reason":"Repeated noise","change":{"action":"set_server_mute","account_id":member,"muted":true}});
    let result = command(&app, &ot, request.clone(), StatusCode::OK).await;
    assert!(
        result["state"]["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["account_id"] == json!(member))
            .unwrap()["server_muted"]
            .as_bool()
            .unwrap()
    );
    command(&app, &ot, request, StatusCode::CONFLICT).await;
    state = snapshot(&app, &ot, &guild).await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_deafen","account_id":owner,"deafened":true}),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(
        &app,
        &mt,
        &mut state,
        json!({"action":"set_server_mute","account_id":owner,"muted":true}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let audit = command(
        &app,
        &ot,
        json!({"action":"audit_log","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(audit["page"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(audit["page"]["items"][0]["reason"], "Repeated noise");
    for token in [&mt, &xt] {
        command(
            &app,
            token,
            json!({"action":"audit_log","guild_id":guild}),
            StatusCode::FORBIDDEN,
        )
        .await;
    }
    diesel::sql_query("INSERT INTO guild_audit_log(guild_id,actor,target,action,reason,details) SELECT $1::uuid,$2::uuid,$3::uuid,'test','','' FROM generate_series(1,1005)").bind::<Text,_>(guild.as_str().unwrap()).bind::<Text,_>(owner.to_string()).bind::<Text,_>(member.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_mute","account_id":member,"muted":false}),
        StatusCode::OK,
    )
    .await;
    let mut next = Value::Null;
    let mut count = 0;
    loop {
        let page = command(
            &app,
            &ot,
            json!({"action":"audit_log","guild_id":guild,"page":{"limit":100,"after":next}}),
            StatusCode::OK,
        )
        .await;
        count += page["page"]["items"].as_array().unwrap().len();
        next = page["page"]["next_cursor"].clone();
        if next.is_null() {
            break;
        }
    }
    assert_eq!(count, 1000);
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"set_server_deafen","account_id":member,"deafened":true}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"transfer_owner","account_id":member}),
        StatusCode::OK,
    )
    .await;
    let moderation = command(
        &app,
        &mt,
        json!({"action":"inspect_moderation","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    let promoted = moderation["state"]["members"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["account_id"] == json!(member))
        .unwrap();
    assert_eq!(promoted["server_deafened"], false);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn invite_redemptions_serialize_at_current_owner_member_quota() {
    let db = database();
    let (_, ot) = user(&db, "invite_quota_owner");
    let (_, at) = user(&db, "invite_quota_a");
    let (_, bt) = user(&db, "invite_quota_b");
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Invite quota"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let first = invite(&app, &ot, &mut state, 20).await;
    let second = invite(&app, &ot, &mut state, 20).await;
    diesel::sql_query("UPDATE instance_role_limits SET members=2 WHERE builtin AND role='user'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let (one, two) = tokio::join!(
        call(
            &app,
            PERMISSIONS_PATH,
            &at,
            json!({"action":"accept_invite","code":first["code"]})
        ),
        call(
            &app,
            PERMISSIONS_PATH,
            &bt,
            json!({"action":"accept_invite","code":second["code"]})
        )
    );
    assert_eq!(
        [one.0, two.0]
            .into_iter()
            .filter(StatusCode::is_success)
            .count(),
        1
    );
    assert!([one.0, two.0].contains(&StatusCode::CONFLICT));
    let list = command(
        &app,
        &ot,
        json!({"action":"list_invites","guild_id":state["guild"]["id"]}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        list["invites"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["uses"].as_u64().unwrap())
            .sum::<u64>(),
        1
    );
}
