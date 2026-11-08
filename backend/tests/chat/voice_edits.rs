//! Voice messages, spoiler marks and bounded edit history (guild and private).
use super::friends::social;
use super::*;
use thiscord_shared::{
    friends::DIRECT_ATTACHMENTS_PATH,
    safety::SAFETY_PATH,
    voice_message::{
        MAX_VOICE_MESSAGE_BYTES, OggOpusWriter, VOICE_MESSAGE_FILENAME, WAVEFORM_BARS,
        encode_waveform,
    },
};

/// Container-valid Ogg Opus with 20 ms CELT packets; the backend never decodes.
fn voice(milliseconds: u32) -> Vec<u8> {
    let mut writer = OggOpusWriter::new(3, 1, 312);
    for i in 0..milliseconds / 20 {
        let mut packet = vec![0xF8];
        packet.extend(std::iter::repeat_n(i as u8, 40));
        writer.push(&packet, 960);
    }
    writer.finish()
}

fn waveform() -> String {
    encode_waveform(&(0..WAVEFORM_BARS as u8).map(|v| v * 4).collect::<Vec<_>>())
}

async fn post(
    app: &Router,
    token: &str,
    uri: String,
    body: Vec<u8>,
    expected: StatusCode,
) -> Value {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(uri)
                .header("authorization", format!("Bearer {token}"))
                .header("content-length", body.len())
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 32768).await.unwrap()).unwrap();
    assert_eq!(status, expected, "{body}");
    body
}

fn guild_upload(guild: &Value, channel: &Value, voice: bool, waveform: Option<&str>) -> String {
    let mut query = url::form_urlencoded::Serializer::new(String::new());
    query
        .append_pair("guild_id", guild.as_str().unwrap())
        .append_pair("channel_id", channel.as_str().unwrap())
        .append_pair("filename", "recording.webm")
        .append_pair("content_type", "audio/webm");
    if voice {
        query.append_pair("voice", "true");
    }
    if let Some(waveform) = waveform {
        query.append_pair("waveform", waveform);
    }
    format!("{ATTACHMENTS_PATH}?{}", query.finish())
}

async fn fetch(app: &Router, token: &str, path: String, expected: StatusCode) -> Vec<u8> {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(path)
                .header("authorization", format!("Bearer {token}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), expected);
    if expected.is_success() {
        assert_eq!(response.headers()["content-type"], "audio/ogg");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    }
    to_bytes(response.into_body(), MAX_VOICE_MESSAGE_BYTES + 4096)
        .await
        .unwrap()
        .to_vec()
}

fn count(db: &Database, sql: &str, id: &Value) -> i64 {
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        n: i64,
    }
    diesel::sql_query(sql)
        .bind::<Text, _>(id.as_str().unwrap())
        .get_result::<Count>(&mut db.pool.get().unwrap())
        .unwrap()
        .n
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn voice_uploads_are_validated_and_spoilers_publish_with_the_message() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, _, state, channel) = setup(&db).await;
    let guild = state["guild"]["id"].clone();
    let wave = waveform();
    // Waveforms belong only to voice uploads; bodies must be bounded Ogg Opus.
    let plain_wave = guild_upload(&guild, &channel, false, Some(&wave));
    post(
        &app,
        &owner,
        plain_wave,
        voice(2000),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let uri = guild_upload(&guild, &channel, true, Some(&wave));
    for body in [b"OggS not really".to_vec(), voice(200), voice(302_000)] {
        post(&app, &owner, uri.clone(), body, StatusCode::BAD_REQUEST).await;
    }
    let mut oversized = voice(2000);
    oversized.resize(MAX_VOICE_MESSAGE_BYTES + 1, 0);
    post(
        &app,
        &owner,
        uri.clone(),
        oversized,
        StatusCode::BAD_REQUEST,
    )
    .await;
    let bad_wave = guild_upload(&guild, &channel, true, Some("AAAA"));
    post(&app, &owner, bad_wave, voice(2000), StatusCode::BAD_REQUEST).await;
    // Valid: duration comes from the file, the name and type are canonical.
    let bytes = voice(3000);
    let recorded = post(&app, &owner, uri.clone(), bytes.clone(), StatusCode::OK).await;
    assert_eq!(recorded["filename"], VOICE_MESSAGE_FILENAME);
    assert_eq!(recorded["content_type"], "audio/ogg");
    assert_eq!(
        recorded["voice"]["duration_ms"],
        (150 * 960 - 312) * 1000 / 48_000
    );
    assert_eq!(recorded["voice"]["waveform"], wave);
    assert!(recorded.get("spoiler").is_none());
    // Without a waveform the outline is flat rather than client-invented.
    let flat = post(
        &app,
        &owner,
        guild_upload(&guild, &channel, true, None),
        voice(1000),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        flat["voice"]["waveform"],
        encode_waveform(&[0; WAVEFORM_BARS])
    );
    let picture = post(
        &app,
        &owner,
        guild_upload(&guild, &channel, false, None),
        b"\x89PNG\r\n\x1a\nimage".to_vec(),
        StatusCode::OK,
    )
    .await;
    assert!(picture.get("voice").is_none());

    // Spoilers must name attachments of the same send.
    let client = Uuid::new_v4();
    let mut send = json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":client,"content":"","attachment_ids":[recorded["id"],picture["id"]],"spoilers":[flat["id"]]});
    chat(&app, &owner, send.clone(), StatusCode::BAD_REQUEST).await;
    send["spoilers"] = json!([picture["id"], picture["id"]]);
    chat(&app, &owner, send.clone(), StatusCode::BAD_REQUEST).await;
    send["spoilers"] = json!([picture["id"]]);
    let message = chat(&app, &owner, send.clone(), StatusCode::OK).await["message"].clone();
    let attachments = message["attachments"].as_array().unwrap();
    assert_eq!(attachments[0]["voice"], recorded["voice"]);
    assert!(attachments[0].get("spoiler").is_none());
    assert_eq!(attachments[1]["spoiler"], true);
    // Exact retries return the message; changed spoiler marks conflict.
    assert_eq!(
        chat(&app, &owner, send.clone(), StatusCode::OK).await["message"]["id"],
        message["id"]
    );
    send["spoilers"] = json!([]);
    chat(&app, &owner, send, StatusCode::CONFLICT).await;
    // Members read the stored flags and download the original bytes.
    let history = chat(
        &app,
        &guest,
        json!({"action":"history","guild_id":guild,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        history["history"]["messages"][0]["attachments"],
        message["attachments"]
    );
    let path = format!("{ATTACHMENTS_PATH}/{}", recorded["id"].as_str().unwrap());
    assert_eq!(
        fetch(&app, &guest, path.clone(), StatusCode::OK).await,
        bytes
    );
    // Deleting the message removes the voice message bytes.
    chat(&app, &owner, json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":message["id"],"revision":message["revision"]}), StatusCode::OK).await;
    fetch(&app, &guest, path, StatusCode::FORBIDDEN).await;
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn guild_edit_history_is_bounded_authorized_and_deleted_with_the_message() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (app, owner, guest, guest_id, mut state, channel) = setup(&db).await;
    let (_, outsider) = user(&db, "outsider");
    let guild = state["guild"]["id"].clone();
    let mut message = chat(&app, &owner, json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"v0"}), StatusCode::OK).await["message"].clone();
    for version in 1..=22 {
        message = chat(&app, &owner, json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":message["id"],"revision":message["revision"],"content":format!("v{version}")}), StatusCode::OK).await["message"].clone();
    }
    // An unchanged edit is not a new version.
    chat(&app, &owner, json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":message["id"],"revision":message["revision"],"content":"v22"}), StatusCode::OK).await;
    let request = json!({"action":"edit_history","guild_id":guild,"channel_id":channel,"message_id":message["id"]});
    let history = chat(&app, &guest, request.clone(), StatusCode::OK).await["history"].clone();
    let versions = history["versions"].as_array().unwrap();
    assert_eq!(history["message_id"], message["id"]);
    assert_eq!(versions.len(), MAX_EDIT_HISTORY);
    assert_eq!(versions[0]["content"], "v21");
    assert_eq!(versions[MAX_EDIT_HISTORY - 1]["content"], "v2");
    for pair in versions.windows(2) {
        let newer: thiscord_shared::Timestamp =
            serde_json::from_value(pair[0]["created_at"].clone()).unwrap();
        let older: thiscord_shared::Timestamp =
            serde_json::from_value(pair[1]["replaced_at"].clone()).unwrap();
        assert_eq!(
            newer, older,
            "each version starts when the previous one was replaced"
        );
    }
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM message_edits WHERE message_id=$1::uuid",
            &message["id"]
        ),
        MAX_EDIT_HISTORY as i64
    );

    // Same access as reading the message: membership, channel scope, ReadHistory.
    chat(&app, &outsider, request.clone(), StatusCode::FORBIDDEN).await;
    let mut wrong = request.clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"two","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    wrong["channel_id"] = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] != channel)
        .unwrap()["id"]
        .clone();
    chat(&app, &guest, wrong, StatusCode::FORBIDDEN).await;
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":["read_history"]}),StatusCode::OK).await;
    chat(&app, &guest, request.clone(), StatusCode::FORBIDDEN).await;
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":guest_id},"allow":[],"deny":[]}),StatusCode::OK).await;
    chat(&app, &guest, request.clone(), StatusCode::OK).await;

    // Reply previews mask spoilers before the 160-character cut.
    let secret = chat(&app, &owner, json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":format!("{} ||the butler did it||", "a".repeat(150))}), StatusCode::OK).await["message"].clone();
    let reply = chat(&app, &guest, json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"no way","reply_to":secret["id"]}), StatusCode::OK).await["message"].clone();
    let preview = reply["reply"]["preview"].as_str().unwrap();
    assert!(
        !preview.contains("butler") && preview.contains(SPOILER_PLACEHOLDER),
        "{preview}"
    );

    // Deleting the message deletes its history; it is never readable afterwards.
    chat(&app, &owner, json!({"action":"delete","guild_id":guild,"channel_id":channel,"message_id":message["id"],"revision":message["revision"]}), StatusCode::OK).await;
    chat(&app, &guest, request.clone(), StatusCode::FORBIDDEN).await;
    chat(&app, &owner, request, StatusCode::FORBIDDEN).await;
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM message_edits WHERE message_id=$1::uuid",
            &message["id"]
        ),
        0
    );
    // Channel deletion cascades remaining history rows.
    let kept = chat(&app, &owner, json!({"action":"send","guild_id":guild,"channel_id":channel,"client_id":Uuid::new_v4(),"content":"a"}), StatusCode::OK).await["message"].clone();
    chat(&app, &owner, json!({"action":"edit","guild_id":guild,"channel_id":channel,"message_id":kept["id"],"revision":kept["revision"],"content":"b"}), StatusCode::OK).await;
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM message_edits WHERE message_id=$1::uuid",
            &kept["id"]
        ),
        1
    );
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"delete_channel","channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM message_edits WHERE message_id=$1::uuid",
            &kept["id"]
        ),
        0
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn private_edit_history_follows_pair_access_blocks_deletion_and_account_removal() {
    let _guard = TEST_LOCK.lock().await;
    let db = database();
    let (aid, a) = user(&db, "hist_a");
    let (bid, b) = user(&db, "hist_b");
    let (_, outsider) = user(&db, "hist_out");
    let conversation = Uuid::new_v4();
    diesel::sql_query("INSERT INTO friendships(id,low_id,high_id,requester,state) VALUES($1::uuid,LEAST($2::uuid,$3::uuid),GREATEST($2::uuid,$3::uuid),$2::uuid,'accepted')").bind::<Text,_>(conversation.to_string()).bind::<Text,_>(aid.to_string()).bind::<Text,_>(bid.to_string()).execute(&mut db.pool.get().unwrap()).unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);

    // A private voice message with a spoiler cover.
    let uri = format!(
        "{DIRECT_ATTACHMENTS_PATH}?conversation_id={conversation}&filename=x&content_type=audio%2Fogg&voice=true&waveform={}",
        waveform()
    );
    post(
        &app,
        &outsider,
        uri.clone(),
        voice(1500),
        StatusCode::FORBIDDEN,
    )
    .await;
    post(
        &app,
        &a,
        uri.replace("voice=true&", ""),
        voice(1500),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let file = post(&app, &a, uri, voice(1500), StatusCode::OK).await;
    assert_eq!(
        file["voice"]["duration_ms"],
        (75 * 960 - 312) * 1000 / 48_000
    );
    let sent = social(&app, &a, json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"","attachments":[file["id"]],"spoilers":[file["id"]]}), StatusCode::OK).await["message"].clone();
    assert_eq!(sent["attachments"][0]["spoiler"], true);
    assert_eq!(sent["attachments"][0]["voice"], file["voice"]);
    let path = format!("{DIRECT_ATTACHMENTS_PATH}/{}", file["id"].as_str().unwrap());
    assert_eq!(
        fetch(&app, &b, path.clone(), StatusCode::OK).await,
        voice(1500)
    );
    fetch(&app, &outsider, path, StatusCode::FORBIDDEN).await;

    let mut message = social(&app, &a, json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"first"}), StatusCode::OK).await["message"].clone();
    for content in ["second", "third", "fourth"] {
        message = social(&app, &a, json!({"action":"edit","conversation_id":conversation,"message_id":message["id"],"revision":message["revision"],"content":content}), StatusCode::OK).await["message"].clone();
    }
    let request =
        json!({"action":"edit_history","conversation_id":conversation,"message_id":message["id"]});
    let history = social(&app, &b, request.clone(), StatusCode::OK).await["history"].clone();
    let contents: Vec<_> = history["versions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["content"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(contents, ["third", "second", "first"]);
    social(&app, &outsider, request.clone(), StatusCode::FORBIDDEN).await;
    let mut foreign = request.clone();
    foreign["conversation_id"] = json!(Uuid::new_v4());
    social(&app, &a, foreign, StatusCode::FORBIDDEN).await;

    // A block in either direction removes access for both; unblocking alone
    // does not restore it, a newly accepted request does (history retained).
    let (status, _) = call(
        &app,
        SAFETY_PATH,
        &b,
        json!({"action":"block","account_id":aid}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    social(&app, &a, request.clone(), StatusCode::FORBIDDEN).await;
    social(&app, &b, request.clone(), StatusCode::FORBIDDEN).await;
    let (status, _) = call(
        &app,
        SAFETY_PATH,
        &b,
        json!({"action":"unblock","account_id":aid}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    social(&app, &b, request.clone(), StatusCode::FORBIDDEN).await;
    social(
        &app,
        &a,
        json!({"action":"add","account_id":bid}),
        StatusCode::OK,
    )
    .await;
    social(
        &app,
        &b,
        json!({"action":"accept","account_id":aid}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        social(&app, &b, request.clone(), StatusCode::OK).await["history"]["versions"]
            .as_array()
            .unwrap()
            .len(),
        3
    );

    // Deleting the message deletes its history.
    social(&app, &a, json!({"action":"delete","conversation_id":conversation,"message_id":message["id"],"revision":message["revision"]}), StatusCode::OK).await;
    social(&app, &b, request, StatusCode::FORBIDDEN).await;
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM direct_message_edits WHERE message_id=$1::uuid",
            &message["id"]
        ),
        0
    );

    // Account deletion cascades the private history with the conversation.
    let other = social(&app, &a, json!({"action":"send","conversation_id":conversation,"client_id":Uuid::new_v4(),"content":"x"}), StatusCode::OK).await["message"].clone();
    social(&app, &a, json!({"action":"edit","conversation_id":conversation,"message_id":other["id"],"revision":other["revision"],"content":"y"}), StatusCode::OK).await;
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM direct_message_edits WHERE message_id=$1::uuid",
            &other["id"]
        ),
        1
    );
    diesel::sql_query("DELETE FROM accounts WHERE id=$1::uuid")
        .bind::<Text, _>(aid.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    assert_eq!(
        count(
            &db,
            "SELECT count(*) AS n FROM direct_message_edits WHERE message_id=$1::uuid",
            &other["id"]
        ),
        0
    );
}
