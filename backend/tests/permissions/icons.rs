use super::*;
use base64::engine::general_purpose::STANDARD;

fn png(width: u32, height: u32) -> String {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(width, height)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    STANDARD.encode(bytes.into_inner())
}
async fn set_icon(
    app: &Router,
    token: &str,
    guild: &Value,
    revision: &Value,
    image: Option<String>,
    expected: StatusCode,
) -> Value {
    let (status, body) = call(
        app,
        GUILD_ICON_PATH,
        token,
        json!({"guild_id":guild,"revision":revision,"image_base64":image}),
    )
    .await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn fetch(app: &Router, icon: &Value) -> (StatusCode, Vec<u8>) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{GUILD_ICON_PATH}/{}", icon.as_str().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    if status.is_success() {
        assert_eq!(response.headers()["content-type"], "image/png");
        assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    }
    let body = to_bytes(response.into_body(), 1_000_000).await.unwrap();
    (status, body.to_vec())
}
async fn listed_icon(app: &Router, token: &str, guild: &Value) -> Value {
    let guilds = command(app, token, json!({"action":"list_guilds"}), StatusCode::OK).await;
    guilds["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == *guild)
        .map(|g| g.get("icon_id").cloned().unwrap_or(Value::Null))
        .unwrap()
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn guild_icons_require_manage_guild_revision_and_membership() {
    let db = database();
    let (_, owner) = user(&db, "icon_owner");
    let (_, member) = user(&db, "icon_member");
    let (_, admin) = user(&db, "icon_admin");
    let (manager, manager_token) = user(&db, "icon_manager");
    permissions::bootstrap_owner(&db.pool, "icon_admin").unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Icons"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    assert!(state["guild"].get("icon_id").is_none_or(Value::is_null));
    for username in ["icon_member", "icon_manager"] {
        change(
            &app,
            &owner,
            &mut state,
            json!({"action":"add_member","username":username}),
            StatusCode::OK,
        )
        .await;
    }
    change(&app,&owner,&mut state,json!({"action":"create_role","name":"Managers","position":5,"permissions":["manage_guild"]}),StatusCode::OK).await;
    let role = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Managers")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"assign_role","account_id":manager,"role_id":role,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    let revision = state["guild"]["revision"].clone();

    // Members without ManageGuild and non-members (even the instance owner) are rejected.
    set_icon(
        &app,
        &member,
        &guild,
        &revision,
        Some(png(64, 64)),
        StatusCode::FORBIDDEN,
    )
    .await;
    set_icon(
        &app,
        &admin,
        &guild,
        &revision,
        Some(png(64, 64)),
        StatusCode::FORBIDDEN,
    )
    .await;
    set_icon(
        &app,
        "",
        &guild,
        &revision,
        Some(png(64, 64)),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    // Invalid images are rejected before any write; the revision is unchanged.
    set_icon(
        &app,
        &owner,
        &guild,
        &revision,
        Some(STANDARD.encode(b"not an image")),
        StatusCode::BAD_REQUEST,
    )
    .await;
    set_icon(
        &app,
        &owner,
        &guild,
        &revision,
        Some("%%%".into()),
        StatusCode::BAD_REQUEST,
    )
    .await;
    set_icon(
        &app,
        &owner,
        &guild,
        &revision,
        Some(png(4097, 1)),
        StatusCode::BAD_REQUEST,
    )
    .await;
    let (status, _) = call(
        &app,
        GUILD_ICON_PATH,
        &owner,
        json!({"guild_id":guild,"revision":revision,"image_base64":null,"path":"/etc"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // A ManageGuild role holder sets a normalized square icon and bumps the revision.
    let updated = set_icon(
        &app,
        &manager_token,
        &guild,
        &revision,
        Some(png(300, 120)),
        StatusCode::OK,
    )
    .await;
    assert_eq!(updated["id"], guild);
    assert_eq!(updated["revision"], revision.as_i64().unwrap() + 1);
    let first = updated["icon_id"].clone();
    assert!(first.is_string());
    let (status, bytes) = fetch(&app, &first).await;
    assert_eq!(status, StatusCode::OK);
    let decoded = image::load_from_memory(&bytes).unwrap();
    assert_eq!((decoded.width(), decoded.height()), (256, 256));
    // Members see the icon in their guild list and home view.
    assert_eq!(listed_icon(&app, &member, &guild).await, first);
    let home = command(
        &app,
        &member,
        json!({"action":"view_guild","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(home["home"]["guild"]["icon_id"], first);

    // Stale revisions conflict and leave the icon untouched.
    set_icon(&app, &owner, &guild, &revision, None, StatusCode::CONFLICT).await;
    assert_eq!(listed_icon(&app, &owner, &guild).await, first);

    // Replacement issues a new opaque ID; the old URL stops resolving.
    let replaced = set_icon(
        &app,
        &owner,
        &guild,
        &updated["revision"],
        Some(png(32, 32)),
        StatusCode::OK,
    )
    .await;
    let second = replaced["icon_id"].clone();
    assert_ne!(first, second);
    assert_eq!(fetch(&app, &first).await.0, StatusCode::NOT_FOUND);
    assert_eq!(fetch(&app, &second).await.0, StatusCode::OK);

    // Removal clears the ID for everyone and deletes the stored image.
    let removed = set_icon(
        &app,
        &owner,
        &guild,
        &replaced["revision"],
        None,
        StatusCode::OK,
    )
    .await;
    assert!(removed.get("icon_id").is_none_or(Value::is_null));
    assert_eq!(
        removed["revision"],
        replaced["revision"].as_i64().unwrap() + 1
    );
    assert_eq!(fetch(&app, &second).await.0, StatusCode::NOT_FOUND);
    assert!(listed_icon(&app, &member, &guild).await.is_null());
    // Removing an absent icon is a no-op that does not advance the revision.
    let unchanged = set_icon(
        &app,
        &owner,
        &guild,
        &removed["revision"],
        None,
        StatusCode::OK,
    )
    .await;
    assert_eq!(unchanged["revision"], removed["revision"]);
    let (status, _) = fetch(&app, &json!("not-a-uuid")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Guild deletion cascades the stored image.
    let third = set_icon(
        &app,
        &owner,
        &guild,
        &unchanged["revision"],
        Some(png(16, 16)),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        &owner,
        json!({"action":"change","guild_id":guild,"revision":third["revision"],"change":{"action":"delete"}}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        fetch(&app, &third["icon_id"]).await.0,
        StatusCode::NOT_FOUND
    );
    let remaining: i64 = diesel::select(diesel::dsl::sql::<diesel::sql_types::BigInt>(
        "(SELECT count(*) FROM guild_icons)",
    ))
    .get_result(&mut db.pool.get().unwrap())
    .unwrap();
    assert_eq!(remaining, 0);

    // Rollback drops only the icon table; reapplying restores an empty table.
    let mut c = db.pool.get().unwrap();
    c.batch_execute(include_str!(
        "../../migrations/20261103200000_guild_icons/down.sql"
    ))
    .unwrap();
    c.batch_execute(include_str!(
        "../../migrations/20261103200000_guild_icons/up.sql"
    ))
    .unwrap();
}
