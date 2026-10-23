use super::*;

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn mention_search_matches_public_names_and_handles_with_guild_isolation() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let (_, outsider) = user(&db, "outsider");
    // Duplicate public names must remain separate, identifiable candidates.
    diesel::sql_query("UPDATE accounts SET display_name='Ada Lovelace 🎮'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let request = |query: &str| json!({"action":"mention_suggestions", "guild_id":guild, "channel_id":channel, "query":query});
    let result = chat(&app, &guest, request("ADA LOVE"), StatusCode::OK).await;
    let members = result["members"].as_array().unwrap();
    assert_eq!(members.len(), 2); // Offline users are included; outsider is excluded.
    assert_eq!(members[0]["username"], "guest");
    assert_eq!(members[1]["username"], "owner");
    assert_ne!(members[0]["account_id"], members[1]["account_id"]);
    for member in members {
        assert_eq!(member["display_name"], "Ada Lovelace 🎮");
        assert_eq!(member.as_object().unwrap().len(), 3); // No credentials or email.
    }
    let result = chat(&app, &owner, request("GUE"), StatusCode::OK).await;
    assert_eq!(result["members"].as_array().unwrap().len(), 1);
    assert_eq!(result["members"][0]["account_id"], json!(guest_id));
    for query in ["🎮", "ada lovelace"] {
        let result = chat(&app, &owner, request(query), StatusCode::OK).await;
        assert_eq!(result["members"].as_array().unwrap().len(), 2);
    }
    for query in ["%", "_", "outsider", "' OR TRUE --"] {
        let result = chat(&app, &owner, request(query), StatusCode::OK).await;
        assert!(result["members"].as_array().unwrap().is_empty());
    }
    for query in ["x".repeat(65), "a\nb".into()] {
        chat(&app, &owner, request(&query), StatusCode::BAD_REQUEST).await;
    }
    chat(&app, &outsider, request(""), StatusCode::FORBIDDEN).await;
    chat(&app, "", request(""), StatusCode::UNAUTHORIZED).await;
    // A handle selected using the public name resolves to the intended account.
    let sent = chat(&app, &owner, json!({"action":"send", "guild_id":guild, "channel_id":channel,
        "client_id":Uuid::new_v4(), "content":format!("Hello @{}", members[0]["username"].as_str().unwrap())}), StatusCode::OK).await;
    assert_eq!(sent["message"]["mentions"], json!([guest_id]));
    for permission in ["view_channel", "send_messages"] {
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"set_override", "channel_id":channel,
            "target":{"kind":"member", "id":guest_id}, "allow":[], "deny":[permission]}),
            StatusCode::OK,
        )
        .await;
        chat(&app, &guest, request(""), StatusCode::FORBIDDEN).await;
    }
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member", "account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    chat(&app, &guest, request(""), StatusCode::FORBIDDEN).await;
    assert!(
        chat(&app, &owner, request("guest"), StatusCode::OK).await["members"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn mention_search_is_bounded_and_ranks_prefixes_first() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, _, _, mut state, channel) = setup(&db).await;
    for i in 0..10 {
        let name = format!("member_{i}");
        user(&db, &name);
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"add_member", "username":name}),
            StatusCode::OK,
        )
        .await;
    }
    diesel::sql_query("UPDATE accounts SET display_name=CASE WHEN username='member_9' THEN 'Target' ELSE 'A target' END")
        .execute(&mut db.pool.get().unwrap()).unwrap();
    let result = chat(
        &app,
        &owner,
        json!({"action":"mention_suggestions", "guild_id":state["guild"]["id"],
        "channel_id":channel, "query":"target"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        result["members"].as_array().unwrap().len(),
        MENTION_SUGGESTION_LIMIT
    );
    assert_eq!(result["members"][0]["username"], "member_9");
}
