use super::*;
use thiscord_shared::search::SEARCH_PATH;

async fn send_text(app: &Router, token: &str, guild: &Value, channel: &Value, text: &str) -> Value {
    chat(app,token,json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":text}),StatusCode::OK).await["message"].clone()
}
async fn search(
    app: &Router,
    token: &str,
    scope: Value,
    cursor: Value,
    person: Value,
    limit: u16,
) -> Value {
    let (status,body)=call(app,SEARCH_PATH,token,json!({"action":"search","filters":{"scope":scope,"query":"needle","person":person,"after":null,"before":null},"cursor":cursor,"limit":limit})).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn rich_replies_reactions_pins_and_socket_updates() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let target = send_text(
        &app,
        &owner,
        &guild,
        &channel,
        "needle original private preview",
    )
    .await;
    let request = json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"reply","reply_to":target["id"]});
    let first = chat(&app, &guest, request.clone(), StatusCode::OK).await;
    let retry = chat(&app, &guest, request.clone(), StatusCode::OK).await;
    assert_eq!(first["message"]["id"], retry["message"]["id"]);
    assert_eq!(first["message"]["reply"]["preview"], target["content"]);
    let mut changed = request.clone();
    changed["reply_to"] = Value::Null;
    chat(&app, &guest, changed, StatusCode::CONFLICT).await;
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"elsewhere","kind":"text"}),
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
    let mut cross = request.clone();
    cross["client_id"] = json!(Uuid::new_v4());
    cross["channel_id"] = other.clone();
    chat(&app, &guest, cross, StatusCode::FORBIDDEN).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = app.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let mut socket = open_socket(address, &owner, &guild, &channel).await;
    event(&mut socket, "ready").await;
    let reaction = json!({"action":"react","guild_id":guild,"channel_id":channel,"message_id":target["id"],"emoji":"👍","present":true});
    let (a, b) = tokio::join!(
        chat(&app, &guest, reaction.clone(), StatusCode::OK),
        chat(&app, &guest, reaction.clone(), StatusCode::OK)
    );
    assert_eq!(a["message"]["revision"], b["message"]["revision"]);
    assert_eq!(
        a["message"]["reactions"],
        json!([{"emoji":"👍","count":1,"mine":true}])
    );
    let pushed = event(&mut socket, "message").await;
    assert_eq!(pushed["message"]["reactions"][0]["mine"], false);
    assert_eq!(pushed["message"]["reactions"][0]["count"], 1);
    let mut remove = reaction.clone();
    remove["present"] = json!(false);
    let removed = chat(&app, &guest, remove.clone(), StatusCode::OK).await;
    let removed_again = chat(&app, &guest, remove, StatusCode::OK).await;
    assert_eq!(
        removed["message"]["revision"],
        removed_again["message"]["revision"]
    );
    assert_eq!(removed["message"]["reactions"], json!([]));
    chat(&app, &guest, reaction.clone(), StatusCode::OK).await;
    let own = chat(&app, &owner, reaction.clone(), StatusCode::OK).await;
    assert_eq!(own["message"]["reactions"][0]["count"], 2);
    let pin = json!({"action":"pin","guild_id":guild,"channel_id":channel,"message_id":target["id"],"revision":own["message"]["revision"],"pinned":true});
    chat(&app, &guest, pin.clone(), StatusCode::FORBIDDEN).await;
    let pinned = chat(&app, &owner, pin.clone(), StatusCode::OK).await;
    assert_eq!(pinned["message"]["pinned"], true);
    chat(&app, &owner, pin, StatusCode::CONFLICT).await;
    let pins = chat(
        &app,
        &guest,
        json!({"action":"pins","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(pins["messages"].as_array().unwrap().len(), 1);
    let context = chat(
        &app,
        &guest,
        json!({"action":"context","guild_id":guild,"channel_id":channel,"message_id":target["id"]}),
        StatusCode::OK,
    )
    .await;
    assert!(
        context["history"]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["id"] == target["id"])
    );
    chat(&app,&owner,json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":target["id"],"revision":pinned["message"]["revision"]}),StatusCode::OK).await;
    let history = chat(
        &app,
        &guest,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    let reply = history["history"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == first["message"]["id"])
        .unwrap();
    assert_eq!(reply["reply"]["preview"], Value::Null);
    let deleted = history["history"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == target["id"])
        .unwrap();
    assert_eq!(deleted["reactions"], json!([]));
    assert_eq!(deleted["pinned"], false);
    chat(&app, &guest, reaction, StatusCode::FORBIDDEN).await;
    chat(&app,&guest,json!({"action":"context","guild_id":guild,"channel_id":other,"message_id":first["message"]["id"]}),StatusCode::FORBIDDEN).await;
    let mut deleted_reply = request;
    deleted_reply["client_id"] = json!(Uuid::new_v4());
    chat(&app, &guest, deleted_reply, StatusCode::FORBIDDEN).await;
    socket.close(None).await.unwrap();
    server.abort();
    let _ = server.await;
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn rich_search_keysets_filters_context_and_guild_revocation() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let scope = json!({"kind":"guild","guild_id":guild,"channel_id":null});
    let one = send_text(&app, &owner, &guild, &channel, "needle first").await;
    let two = send_text(&app, &guest, &guild, &channel, "needle second").await;
    let three = send_text(&app, &owner, &guild, &channel, "needle third").await;
    let first = search(&app, &guest, scope.clone(), Value::Null, Value::Null, 1).await;
    assert_eq!(first["messages"][0]["id"], three["id"]);
    let next = search(
        &app,
        &guest,
        scope.clone(),
        first["next"].clone(),
        Value::Null,
        1,
    )
    .await;
    assert_eq!(next["messages"][0]["id"], two["id"]);
    let person = search(
        &app,
        &guest,
        scope.clone(),
        Value::Null,
        json!("@guest"),
        50,
    )
    .await;
    assert_eq!(person["messages"].as_array().unwrap().len(), 1);
    let (status,range)=call(&app,SEARCH_PATH,&guest,json!({"action":"search","filters":{"scope":scope,"query":"needle","person":null,"after":one["created_at"],"before":three["created_at"]},"limit":50})).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(range["messages"].as_array().unwrap().len(), 2);
    let (_, context) = call(
        &app,
        SEARCH_PATH,
        &guest,
        json!({"action":"context","scope":scope,"message_id":two["id"]}),
    )
    .await;
    assert_eq!(context["messages"].as_array().unwrap().len(), 3);
    chat(&app,&owner,json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":one["id"],"revision":0}),StatusCode::OK).await;
    let (status, _) = call(
        &app,
        SEARCH_PATH,
        &guest,
        json!({"action":"context","scope":scope,"message_id":one["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let results = search(&app, &guest, scope.clone(), Value::Null, Value::Null, 50).await;
    assert_eq!(results["messages"].as_array().unwrap().len(), 2);
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":["read_history"]}),StatusCode::OK).await;
    assert!(
        search(&app, &guest, scope.clone(), Value::Null, Value::Null, 50).await["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, _) = call(
        &app,
        SEARCH_PATH,
        &guest,
        json!({"action":"context","scope":scope,"message_id":two["id"]}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":[]}),StatusCode::OK).await;
    let other = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"other"}),
        StatusCode::OK,
    )
    .await["state"]["guild"]["id"]
        .clone();
    let (status,_)=call(&app,SEARCH_PATH,&guest,json!({"action":"context","scope":{"kind":"guild","guild_id":other,"channel_id":null},"message_id":two["id"]})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"remove_member","account_id":guest_id}),
        StatusCode::OK,
    )
    .await;
    for action in [
        json!({"action":"context","scope":scope,"message_id":two["id"]}),
        json!({"action":"search","filters":{"scope":scope,"query":"needle","person":null,"after":null,"before":null}}),
    ] {
        let (status, _) = call(&app, SEARCH_PATH, &guest, action).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
}
#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn rich_private_search_requires_locked_current_consent() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (actor, token) = user(&db, "reader");
    let (other, _) = user(&db, "peer");
    let (_, stranger) = user(&db, "stranger");
    let id = Uuid::new_v4();
    let message = Uuid::new_v4();
    let mut c = db.pool.get().unwrap();
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'accepted')").bind::<Text,_>(id.to_string()).bind::<Text,_>(actor.to_string()).bind::<Text,_>(other.to_string()).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO direct_messages(id,conversation_id,author_id,client_id,content,created_at) VALUES($1::uuid,$2::uuid,$3::uuid,$1::uuid,'needle secret',clock_timestamp())").bind::<Text,_>(message.to_string()).bind::<Text,_>(id.to_string()).bind::<Text,_>(other.to_string()).execute(&mut c).unwrap();
    drop(c);
    let app = api::router(Some(db.pool.clone()), vec![]);
    let scope = json!({"kind":"private","conversation_id":id});
    assert_eq!(
        search(&app, &token, scope.clone(), Value::Null, Value::Null, 50).await["messages"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    let (status, context) = call(
        &app,
        SEARCH_PATH,
        &token,
        json!({"action":"context","scope":scope,"message_id":message}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(context["messages"][0]["id"], json!(message));
    let (status, _) = call(
        &app,
        SEARCH_PATH,
        &stranger,
        json!({"action":"context","scope":scope,"message_id":message}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(
        search(
            &app,
            &stranger,
            json!({"kind":"private","conversation_id":null}),
            Value::Null,
            Value::Null,
            50
        )
        .await["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Exercise the upcoming typed deletion column in this isolated baseline schema too.
    let mut c = db.pool.get().unwrap();
    c.batch_execute("ALTER TABLE direct_messages ADD COLUMN IF NOT EXISTS deleted BOOLEAN NOT NULL DEFAULT false").unwrap();
    diesel::sql_query("UPDATE direct_messages SET deleted=true WHERE id=$1::uuid")
        .bind::<Text, _>(message.to_string())
        .execute(&mut c)
        .unwrap();
    assert!(
        search(&app, &token, scope.clone(), Value::Null, Value::Null, 50).await["messages"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    let (status, _) = call(
        &app,
        SEARCH_PATH,
        &token,
        json!({"action":"context","scope":scope,"message_id":message}),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    diesel::sql_query("UPDATE direct_messages SET deleted=false WHERE id=$1::uuid")
        .bind::<Text, _>(message.to_string())
        .execute(&mut c)
        .unwrap();
    drop(c);
    // The request waits behind the relationship mutation, then observes revoked consent.
    let mut blocker = db.pool.get().unwrap();
    blocker.batch_execute("BEGIN").unwrap();
    diesel::sql_query("UPDATE friendships SET state='removed' WHERE id=$1::uuid")
        .bind::<Text, _>(id.to_string())
        .execute(&mut blocker)
        .unwrap();
    let router = app.clone();
    let credential = token.clone();
    let request = json!({"action":"context","scope":scope,"message_id":message});
    let waiting =
        tokio::spawn(async move { call(&router, SEARCH_PATH, &credential, request).await });
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    assert!(!waiting.is_finished());
    blocker.batch_execute("COMMIT").unwrap();
    assert_eq!(waiting.await.unwrap().0, StatusCode::FORBIDDEN);
    let (status,_)=call(&app,SEARCH_PATH,&token,json!({"action":"search","filters":{"scope":scope,"query":"needle","person":null,"after":null,"before":null}})).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL"]
async fn rich_reply_permissions_and_pin_capacity_are_authoritative() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let target = send_text(&app, &owner, &guild, &channel, "target must not leak").await;
    let request = json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"my reply","reply_to":target["id"]});
    let reply = chat(&app, &guest, request.clone(), StatusCode::OK).await["message"].clone();
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":["read_history"]}),StatusCode::OK).await;
    chat(&app, &guest, request, StatusCode::FORBIDDEN).await;
    for content in ["my reply", "edited reply"] {
        let result=chat(&app,&guest,json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":reply["id"],"revision":0,"content":content}),StatusCode::OK).await;
        assert_eq!(result["message"]["reply"]["preview"], Value::Null);
    }
    chat(&app,&guest,json!({"action":"react","guild_id":guild,"channel_id":channel,"message_id":target["id"],"emoji":"👍","present":true}),StatusCode::FORBIDDEN).await;
    let mut c = db.pool.get().unwrap();
    diesel::sql_query("INSERT INTO messages(id,guild_id,channel_id,author_id,client_id,request_hash,content,pinned) SELECT gen_random_uuid(),m.guild_id,m.channel_id,m.author_id,gen_random_uuid(),'fixture','pinned fixture',true FROM messages m CROSS JOIN generate_series(1,50) WHERE m.id=$1::uuid").bind::<Text,_>(target["id"].as_str().unwrap()).execute(&mut c).unwrap();
    drop(c);
    chat(&app,&owner,json!({"action":"pin","guild_id":guild,"channel_id":channel,"message_id":target["id"],"revision":0,"pinned":true}),StatusCode::BAD_REQUEST).await;
    let pins = chat(
        &app,
        &owner,
        json!({"action":"pins","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(pins["messages"].as_array().unwrap().len(), MAX_CHANNEL_PINS);
}
