use super::*;

fn policy(access: &Value, name: &str) -> Value {
    access["policies"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap()
        .clone()
}
async fn access(app: &Router, token: &str) -> Value {
    command(app, token, json!({"action":"instance"}), StatusCode::OK).await["access"].clone()
}
async fn configure(
    app: &Router,
    token: &str,
    p: &Value,
    limits: Value,
    status: StatusCode,
) -> Value {
    command(app,token,json!({"action":"set_instance_role_limits","role_id":p["id"],"name":p["name"],"revision":p["revision"],"limits":limits}),status).await
}
async fn assign(app: &Router, token: &str, account: AccountId, p: &Value, status: StatusCode) {
    command(
        app,
        token,
        json!({"action":"set_instance_role","account_id":account,"role_id":p["id"]}),
        status,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn extensible_roles_quotas_downgrades_and_protected_authority() {
    let db = database();
    let (owner, ot) = user(&db, "quota_owner");
    let (free, ft) = user(&db, "quota_free");
    let (admin, at) = user(&db, "quota_admin");
    let (_, guest) = user(&db, "quota_guest");
    permissions::bootstrap_owner(&db.pool, "quota_owner").unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let initial = access(&app, &ft).await;
    let free_policy = policy(&initial, "Free");
    assert_eq!(initial["policies"].as_array().unwrap().len(), 1);
    let privileged = access(&app, &ot).await;
    let admin_policy = policy(&privileged, "Administrator");
    let owner_policy = policy(&privileged, "Instance owner");
    assert_eq!(
        free_policy["limits"],
        json!({"owned_guilds":1,"text_channels":2,"voice_channels":2,"members":25,"roles":10})
    );
    assert_eq!(initial["policy_id"], free_policy["id"]);
    assign(&app, &ot, admin, &admin_policy, StatusCode::OK).await;
    // More allocation never implies instance or foreign guild authority.
    let premium =
        json!({"owned_guilds":3,"text_channels":4,"voice_channels":5,"members":8,"roles":6});
    let create =
        json!({"action":"create_instance_role","name":"Supporter","role":"user","limits":premium});
    command(&app, &ft, create.clone(), StatusCode::FORBIDDEN).await;
    command(&app, &at, create.clone(), StatusCode::FORBIDDEN).await;
    let created = command(&app, &ot, create.clone(), StatusCode::OK).await;
    let supporter = policy(&created["access"], "Supporter");
    command(&app, &ot, create, StatusCode::CONFLICT).await;
    command(&app,&ot,json!({"action":"create_instance_role","name":"Second owner","role":"owner","limits":premium}),StatusCode::FORBIDDEN).await;
    assign(&app, &ot, free, &owner_policy, StatusCode::FORBIDDEN).await;
    assign(&app, &ot, owner, &supporter, StatusCode::FORBIDDEN).await;
    assign(&app, &ft, free, &supporter, StatusCode::FORBIDDEN).await;
    assign(&app, &at, free, &supporter, StatusCode::FORBIDDEN).await;
    configure(
        &app,
        &at,
        &free_policy,
        premium.clone(),
        StatusCode::FORBIDDEN,
    )
    .await;
    let mut invalid = premium.clone();
    invalid["voice_channels"] = json!(0);
    configure(&app, &ot, &free_policy, invalid, StatusCode::BAD_REQUEST).await;

    let mut state = command(
        &app,
        &ft,
        json!({"action":"create_guild","name":"Free server"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    let over = command(
        &app,
        &ft,
        json!({"action":"create_guild","name":"Too many"}),
        StatusCode::CONFLICT,
    )
    .await;
    assert_eq!(over["code"], "limit_exceeded");
    for kind in ["text", "voice"] {
        for n in 0..2 {
            change(
                &app,
                &ft,
                &mut state,
                json!({"action":"create_channel","name":format!("{kind}{n}"),"kind":kind}),
                StatusCode::OK,
            )
            .await;
        }
        let over = change(
            &app,
            &ft,
            &mut state,
            json!({"action":"create_channel","name":"excess","kind":kind}),
            StatusCode::CONFLICT,
        )
        .await;
        assert_eq!(over["code"], "limit_exceeded");
    }
    // A delegated guild administrator uses the owner's limits, not their own.
    change(
        &app,
        &ft,
        &mut state,
        json!({"action":"add_member","username":"quota_admin"}),
        StatusCode::OK,
    )
    .await;
    change(&app,&ft,&mut state,json!({"action":"create_role","name":"Guild admin","position":5,"permissions":["administrator"]}),StatusCode::OK).await;
    let guild_role = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Guild admin")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &ft,
        &mut state,
        json!({"action":"assign_role","account_id":admin,"role_id":guild_role,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &at,
        &mut state,
        json!({"action":"create_channel","name":"No bypass","kind":"text"}),
        StatusCode::CONFLICT,
    )
    .await;
    assign(&app, &ot, free, &supporter, StatusCode::OK).await;
    let allocated = access(&app, &ft).await;
    assert_eq!(allocated["role"], "user");
    assert_eq!(allocated["policy_id"], supporter["id"]);
    assert_eq!(allocated["policies"].as_array().unwrap().len(), 1);
    assert_eq!(allocated["owned_guilds"], 1);
    change(
        &app,
        &ft,
        &mut state,
        json!({"action":"create_channel","name":"premium","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(state["capacity"]["role_name"], "Supporter");
    let second = command(
        &app,
        &ft,
        json!({"action":"create_guild","name":"Second"}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        &at,
        json!({"action":"inspect","guild_id":second["state"]["guild"]["id"]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let delete_role =
        json!({"action":"delete_instance_role","role_id":supporter["id"],"revision":0});
    command(&app, &ot, delete_role.clone(), StatusCode::BAD_REQUEST).await;
    // Demotion preserves content and existing access, but no further growth.
    assign(&app, &ot, free, &free_policy, StatusCode::OK).await;
    let current = command(
        &app,
        &ft,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(current["state"]["channels"].as_array().unwrap().len(), 5);
    assert_eq!(current["state"]["capacity"]["role_name"], "Free");
    change(
        &app,
        &ft,
        &mut state,
        json!({"action":"create_channel","name":"blocked","kind":"text"}),
        StatusCode::CONFLICT,
    )
    .await;
    let channel = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["kind"] == "text")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &ft,
        &mut state,
        json!({"action":"delete_channel","channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ft,
        &mut state,
        json!({"action":"rename","name":"Still editable"}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        &ft,
        json!({"action":"create_guild","name":"Still full"}),
        StatusCode::CONFLICT,
    )
    .await;
    let mut second = second["state"].clone();
    change(
        &app,
        &ft,
        &mut second,
        json!({"action":"delete"}),
        StatusCode::OK,
    )
    .await;
    command(&app, &ot, delete_role, StatusCode::OK).await;
    command(
        &app,
        &ot,
        json!({"action":"delete_instance_role","role_id":free_policy["id"],"revision":0}),
        StatusCode::FORBIDDEN,
    )
    .await;

    // Both member admission paths and Everyone-inclusive role capacity enforce the policy.
    let small =
        json!({"owned_guilds":1,"text_channels":2,"voice_channels":2,"members":1,"roles":1});
    configure(&app, &ot, &free_policy, small.clone(), StatusCode::OK).await;
    configure(&app, &ot, &free_policy, small, StatusCode::CONFLICT).await;
    command(
        &app,
        &guest,
        json!({"action":"join_guild","guild_id":guild}),
        StatusCode::CONFLICT,
    )
    .await;
    change(
        &app,
        &ft,
        &mut state,
        json!({"action":"add_member","username":"quota_guest"}),
        StatusCode::CONFLICT,
    )
    .await;
    change(
        &app,
        &ft,
        &mut state,
        json!({"action":"create_role","name":"Extra","position":1,"permissions":[]}),
        StatusCode::CONFLICT,
    )
    .await;
    command(
        &app,
        &ft,
        json!({"action":"join_guild","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    // Expired reauthentication cannot alter role policies or assignments.
    diesel::sql_query("UPDATE sessions SET reauthenticated_at=NULL WHERE account_id=$1::uuid")
        .bind::<Text, _>(owner.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    assign(&app, &ot, free, &admin_policy, StatusCode::FORBIDDEN).await;
    configure(&app, &ot, &free_policy, premium, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn role_assignees_are_owner_only_paginated_and_survive_owner_transfer() {
    let db = database();
    let (_, ot) = user(&db, "listing_owner");
    let (next_owner, nt) = user(&db, "listing_successor");
    let (admin, at) = user(&db, "listing_admin");
    permissions::bootstrap_owner(&db.pool, "listing_owner").unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let initial = access(&app, &ot).await;
    assign(
        &app,
        &ot,
        admin,
        &policy(&initial, "Administrator"),
        StatusCode::OK,
    )
    .await;
    let implicit = command(
        &app,
        &ot,
        json!({"action":"list_instance_role_members","role_id":policy(&initial,"Free")["id"]}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        implicit["page"]["items"],
        json!([{"account_id":next_owner,"username":"listing_successor"}])
    );
    let created=command(&app,&ot,json!({"action":"create_instance_role","name":"Partner","role":"user","limits":policy(&initial,"Free")["limits"]}),StatusCode::OK).await;
    let partner = policy(&created["access"], "Partner");
    let list = json!({"action":"list_instance_role_members","role_id":partner["id"],"page":{"limit":100,"after":null}});
    command(&app, &at, list.clone(), StatusCode::FORBIDDEN).await;
    command(&app, &nt, list.clone(), StatusCode::FORBIDDEN).await;
    diesel::sql_query("INSERT INTO accounts(username,email,display_name,email_verified) SELECT 'listed_'||n,'listed_'||n||'@example.test','Listed '||n,TRUE FROM generate_series(1,105) n").execute(&mut db.pool.get().unwrap()).unwrap();
    diesel::sql_query("INSERT INTO instance_role_assignments(account_id,role_id) SELECT id,$1::uuid FROM accounts WHERE username LIKE 'listed_%'").bind::<Text,_>(partner["id"].as_str().unwrap()).execute(&mut db.pool.get().unwrap()).unwrap();
    let first = command(&app, &ot, list.clone(), StatusCode::OK).await;
    assert_eq!(first["page"]["items"].as_array().unwrap().len(), 100);
    let second=command(&app,&ot,json!({"action":"list_instance_role_members","role_id":partner["id"],"page":{"limit":100,"after":first["page"]["next_cursor"]}}),StatusCode::OK).await;
    assert_eq!(second["page"]["items"].as_array().unwrap().len(), 5);
    assert!(second["page"]["next_cursor"].is_null());
    assert!(
        second["page"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| !first["page"]["items"].as_array().unwrap().contains(m))
    );
    assert!(!first.to_string().contains("email"));
    let transfer = command(
        &app,
        &ot,
        json!({"action":"transfer_instance","account_id":next_owner}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(transfer["access"]["role"], "user");
    assert_eq!(transfer["access"]["policies"].as_array().unwrap().len(), 1);
    command(&app, &ot, list.clone(), StatusCode::FORBIDDEN).await;
    let inherited = command(&app, &nt, list, StatusCode::OK).await;
    assert_eq!(inherited["page"]["items"], first["page"]["items"]);
    let account = serde_json::from_value(first["page"]["items"][0]["account_id"].clone()).unwrap();
    assign(
        &app,
        &nt,
        account,
        &policy(&initial, "Free"),
        StatusCode::OK,
    )
    .await;
    let after=command(&app,&nt,json!({"action":"list_instance_role_members","role_id":partner["id"],"page":{"limit":100,"after":null}}),StatusCode::OK).await;
    assert!(
        !after["page"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["account_id"] == first["page"]["items"][0]["account_id"])
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn custom_admin_authority_and_migration_rollback_preserve_legacy_grants() {
    use diesel_migrations::MigrationHarness;
    let db = database();
    let (_, ot) = user(&db, "custom_owner");
    let (admin, at) = user(&db, "custom_admin");
    permissions::bootstrap_owner(&db.pool, "custom_owner").unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let initial = access(&app, &ot).await;
    let free = policy(&initial, "Free");
    let response=command(&app,&ot,json!({"action":"create_instance_role","name":"Operations","role":"admin","limits":free["limits"]}),StatusCode::OK).await;
    let operations = policy(&response["access"], "Operations");
    assign(&app, &ot, admin, &operations, StatusCode::OK).await;
    assert_eq!(access(&app, &at).await["role"], "admin");
    async fn diagnostics(app: &Router, token: &str) -> StatusCode {
        app.clone()
            .oneshot(
                Request::builder()
                    .uri(thiscord_shared::admin::DIAGNOSTICS_PATH)
                    .header("authorization", format!("Bearer {token}"))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap()
            .status()
    }
    assert_eq!(diagnostics(&app, &at).await, StatusCode::OK);
    assign(&app, &ot, admin, &free, StatusCode::OK).await;
    assert_eq!(diagnostics(&app, &at).await, StatusCode::FORBIDDEN);
    assign(&app, &ot, admin, &operations, StatusCode::OK).await;
    let guild = command(
        &app,
        &at,
        json!({"action":"create_guild","name":"Retained"}),
        StatusCode::OK,
    )
    .await["state"]["guild"]["id"]
        .clone();
    // Later feature migrations must not change which rollback is being exercised.
    let mut connection = db.pool.get().unwrap();
    loop {
        let reverted = connection.revert_last_migration(db::MIGRATIONS).unwrap();
        if reverted.to_string() == "20261023000000" {
            break;
        }
    }
    drop(connection);
    db.pool
        .get()
        .unwrap()
        .run_pending_migrations(db::MIGRATIONS)
        .unwrap();
    let restored = access(&app, &at).await;
    assert_eq!(restored["role"], "admin");
    assert_eq!(restored["policies"].as_array().unwrap().len(), 3);
    assert_eq!(
        restored["policy_id"],
        policy(&restored, "Administrator")["id"]
    );
    command(
        &app,
        &at,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn ownership_transfers_and_concurrent_last_slots_obey_limits() {
    let db = database();
    let (_, ot) = user(&db, "transfer_owner");
    let (free, ft) = user(&db, "transfer_free");
    let (recipient, rt) = user(&db, "recipient");
    permissions::bootstrap_owner(&db.pool, "transfer_owner").unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let (a, b) = tokio::join!(
        call(
            &app,
            PERMISSIONS_PATH,
            &ft,
            json!({"action":"create_guild","name":"A"})
        ),
        call(
            &app,
            PERMISSIONS_PATH,
            &ft,
            json!({"action":"create_guild","name":"B"})
        )
    );
    assert_eq!(
        [a.0, b.0].iter().filter(|s| **s == StatusCode::OK).count(),
        1
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let mut free_state = if a.0 == StatusCode::OK {
        a.1["state"].clone()
    } else {
        b.1["state"].clone()
    };
    // Two writes with the same revision: only one may allocate the last channel.
    change(
        &app,
        &ft,
        &mut free_state,
        json!({"action":"create_channel","kind":"text","name":"first"}),
        StatusCode::OK,
    )
    .await;
    let ch = json!({"action":"change","guild_id":free_state["guild"]["id"],"revision":free_state["guild"]["revision"],"change":{"action":"create_channel","kind":"text","name":"last"}});
    let (a, b) = tokio::join!(
        call(&app, PERMISSIONS_PATH, &ft, ch.clone()),
        call(&app, PERMISSIONS_PATH, &ft, ch)
    );
    assert_eq!(
        [a.0, b.0].iter().filter(|s| **s == StatusCode::OK).count(),
        1
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Transfer"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    for username in ["transfer_free", "recipient"] {
        change(
            &app,
            &ot,
            &mut state,
            json!({"action":"add_member","username":username}),
            StatusCode::OK,
        )
        .await;
    }
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"transfer_owner","account_id":free}),
        StatusCode::CONFLICT,
    )
    .await;
    for n in 0..3 {
        change(
            &app,
            &ot,
            &mut state,
            json!({"action":"create_channel","kind":"voice","name":format!("Voice {n}")}),
            StatusCode::OK,
        )
        .await;
    }
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"transfer_owner","account_id":recipient}),
        StatusCode::CONFLICT,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"delete_channel","channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    // Race receiving ownership against creating a server; exactly one takes the owned slot.
    let transfer = json!({"action":"change","guild_id":state["guild"]["id"],"revision":state["guild"]["revision"],"change":{"action":"transfer_owner","account_id":recipient}});
    let (a, b) = tokio::join!(
        call(&app, PERMISSIONS_PATH, &ot, transfer),
        call(
            &app,
            PERMISSIONS_PATH,
            &rt,
            json!({"action":"create_guild","name":"Race"})
        )
    );
    assert_eq!(
        [a.0, b.0].iter().filter(|s| **s == StatusCode::OK).count(),
        1
    );
    assert_eq!(
        [a.0, b.0]
            .iter()
            .filter(|s| **s == StatusCode::CONFLICT)
            .count(),
        1
    );
    assert_eq!(access(&app, &rt).await["owned_guilds"], 1);
}
