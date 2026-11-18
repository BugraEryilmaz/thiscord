//! Adding a member by username sends an invitation the account must accept.
use super::*;
use futures_util::{SinkExt, StreamExt};
use thiscord_shared::{chat::CHAT_PATH, friends::FRIENDS_SOCKET_PATH, safety::SAFETY_PATH};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

/// Sends `add_member` without the fixture's automatic acceptance.
async fn invite(
    app: &Router,
    token: &str,
    state: &mut Value,
    username: &str,
) -> (StatusCode, Value) {
    let (status, body) = call(
        app,
        PERMISSIONS_PATH,
        token,
        json!({"action":"change","guild_id":state["guild"]["id"],"revision":state["guild"]["revision"],"change":{"action":"add_member","username":username}}),
    )
    .await;
    if body["result"] == "state" {
        *state = body["state"].clone();
    }
    (status, body)
}
async fn respond(app: &Router, token: &str, guild: &Value, accept: bool) -> (StatusCode, Value) {
    let action = if accept {
        "accept_guild_invitation"
    } else {
        "decline_guild_invitation"
    };
    call(
        app,
        PERMISSIONS_PATH,
        token,
        json!({"action":action,"guild_id":guild}),
    )
    .await
}
async fn guild_ids(app: &Router, token: &str) -> Vec<Value> {
    command(app, token, json!({"action":"list_guilds"}), StatusCode::OK).await["guilds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g["id"].clone())
        .collect()
}
fn members(state: &Value) -> Vec<Value> {
    state["members"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["account_id"].clone())
        .collect()
}
async fn safety(app: &Router, token: &str, action: &str, id: AccountId) {
    let (status, body) = call(
        app,
        SAFETY_PATH,
        token,
        json!({"action":action,"account_id":id}),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}
fn pending(db: &Database, guild: &Value, account: AccountId) -> bool {
    #[derive(QueryableByName)]
    struct Count {
        #[diesel(sql_type = diesel::sql_types::BigInt)]
        count: i64,
    }
    diesel::sql_query("SELECT count(*) AS count FROM guild_invitations WHERE guild_id=$1::uuid AND account_id=$2::uuid")
        .bind::<Text, _>(guild.as_str().unwrap())
        .bind::<Text, _>(account.to_string())
        .get_result::<Count>(&mut db.pool.get().unwrap())
        .unwrap()
        .count
        == 1
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
/// A friends socket connected as `token`; its first snapshot is still unread.
async fn open(address: std::net::SocketAddr, token: &str) -> Socket {
    let mut request = format!("ws://{address}{FRIENDS_SOCKET_PATH}")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert("origin", "http://localhost:1420".parse().unwrap());
    let (mut socket, _) = connect_async(request).await.unwrap();
    let connect = json!({"version":1,"event":{"type":"connect","token":token}});
    socket
        .send(Message::Text(connect.to_string().into()))
        .await
        .unwrap();
    socket
}
/// Within three seconds: well under the ten-second periodic refresh.
async fn snapshot(socket: &mut Socket, predicate: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        loop {
            let Message::Text(text) = socket.next().await.unwrap().unwrap() else {
                continue;
            };
            let body: Value = serde_json::from_str(&text).unwrap();
            if body["event"]["type"] == "snapshot" && predicate(&body["event"]["snapshot"]) {
                return body["event"]["snapshot"].clone();
            }
        }
    })
    .await
    .expect("expected a pushed snapshot")
}
/// Serves `app` on a local port for friends sockets.
async fn serve(app: &Router) -> (std::net::SocketAddr, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let service = app.clone();
    let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
    (address, server)
}
/// Whether `socket` stays without a pushed snapshot for half a second.
async fn quiet(socket: &mut Socket) -> bool {
    tokio::time::timeout(
        std::time::Duration::from_millis(500),
        snapshot(socket, |_| true),
    )
    .await
    .is_err()
}
/// Moves an invitation's `created_at` back by `age` (a PostgreSQL interval).
fn age(db: &Database, guild: &Value, account: AccountId, age: &str) {
    let aged = diesel::sql_query("UPDATE guild_invitations SET created_at=created_at-$3::interval WHERE guild_id=$1::uuid AND account_id=$2::uuid")
        .bind::<Text, _>(guild.as_str().unwrap())
        .bind::<Text, _>(account.to_string())
        .bind::<Text, _>(age)
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    assert_eq!(aged, 1);
}
/// Sets the owners' (built-in user role) member limit.
fn member_limit(db: &Database, members: i32) {
    diesel::sql_query("UPDATE instance_role_limits SET members=$1 WHERE builtin AND role='user'")
        .bind::<diesel::sql_types::Integer, _>(members)
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
}
/// The response with the per-change revision normalized, for comparing shapes.
fn shape(body: &Value) -> Value {
    let mut shape = body.clone();
    if shape["state"]["guild"]["revision"].is_number() {
        shape["state"]["guild"]["revision"] = json!(0);
    }
    shape
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn invitations_require_consent_from_the_addressed_account() {
    let db = database();
    let (owner, ot) = user(&db, "inv_owner");
    let (invitee, it) = user(&db, "inv_target");
    let (other_id, other) = user(&db, "inv_other");
    let (_, decliner) = user(&db, "inv_decliner");
    // Instance owner: may own the second server deleted below.
    permissions::bootstrap_owner(&db.pool, "inv_owner").unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Invitations"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"create_channel","name":"chat","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let guild = state["guild"]["id"].clone();
    let channel = state["channels"][0]["id"].clone();

    // Inviting creates no membership, role, guild visibility or profile access.
    let (status, body) = invite(&app, &ot, &mut state, "INV_Target ").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "state");
    assert_eq!(members(&state), vec![json!(owner)]);
    assert!(pending(&db, &guild, invitee));
    assert!(!guild_ids(&app, &it).await.contains(&guild));
    for (token, target) in [(&ot, invitee), (&it, owner)] {
        let (status, _) = call(
            &app,
            CHAT_PATH,
            token,
            json!({"action":"member_profile","guild_id":guild,"account_id":target}),
        )
        .await;
        assert!(!status.is_success(), "{status}");
    }
    command(
        &app,
        &it,
        json!({"action":"view_guild","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let history = json!({"action":"history","guild_id":guild,"channel_id":channel});
    assert_eq!(
        call(&app, CHAT_PATH, &it, history.clone()).await.0,
        StatusCode::FORBIDDEN
    );

    // Repeating while pending is idempotent; inviting a member keeps today's error.
    let (status, _) = invite(&app, &ot, &mut state, "inv_target").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(members(&state), vec![json!(owner)]);
    let (status, _) = invite(&app, &ot, &mut state, "inv_owner").await;
    assert_eq!(status, StatusCode::CONFLICT);
    let (status, _) = invite(&app, &ot, &mut state, "nobody_here").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Only the addressed account can answer: another account (even a member
    // invited elsewhere) can neither accept nor decline it by guild ID.
    let (status, _) = respond(&app, &other, &guild, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // Declining only ever removes the caller's own invitation.
    let (status, _) = respond(&app, &other, &guild, false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(pending(&db, &guild, invitee));
    assert!(!guild_ids(&app, &other).await.contains(&guild));

    // Decline removes it and joins nothing; accepting afterwards fails.
    invite(&app, &ot, &mut state, "inv_decliner").await;
    let (status, body) = respond(&app, &decliner, &guild, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "done");
    let (status, _) = respond(&app, &decliner, &guild, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!guild_ids(&app, &decliner).await.contains(&guild));

    // Accepting joins through the ordinary path: membership, revision, access.
    let revision = state["guild"]["revision"].as_i64().unwrap();
    let (status, body) = respond(&app, &it, &guild, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["result"], "joined");
    assert_eq!(body["guild"]["id"], guild);
    assert_eq!(body["guild"]["revision"], revision + 1);
    assert!(!pending(&db, &guild, invitee));
    assert!(guild_ids(&app, &it).await.contains(&guild));
    assert_eq!(call(&app, CHAT_PATH, &it, history).await.0, StatusCode::OK);
    // The invitation was consumed.
    let (status, _) = respond(&app, &it, &guild, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Guild deletion cascades pending invitations.
    let mut doomed = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Doomed"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    invite(&app, &ot, &mut doomed, "inv_other").await;
    let doomed_id = doomed["guild"]["id"].clone();
    assert!(guild_ids(&app, &other).await.is_empty());
    assert!(pending(&db, &doomed_id, other_id));
    change(
        &app,
        &ot,
        &mut doomed,
        json!({"action":"delete"}),
        StatusCode::OK,
    )
    .await;
    assert!(!pending(&db, &doomed_id, other_id));
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn blocks_bans_and_capacity_are_rechecked_without_an_oracle() {
    let db = database();
    user(&db, "cap_root");
    permissions::bootstrap_owner(&db.pool, "cap_root").unwrap();
    let (owner, ot) = user(&db, "cap_owner");
    let (blocker, bt) = user(&db, "cap_blocker");
    let (blocked, blt) = user(&db, "cap_blocked");
    let (first, ft) = user(&db, "cap_first");
    let (second, st) = user(&db, "cap_second");
    let (banned, bant) = user(&db, "cap_banned");
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Rechecks"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();

    // A block in either direction delivers nothing, with an identical response
    // shape to a delivered invitation (state, members unchanged, one revision).
    // The row is stored all the same, so it takes the same slot as any other.
    safety(&app, &bt, "block", owner).await;
    safety(&app, &ot, "block", blocked).await;
    let mut shapes = vec![];
    for username in ["cap_blocker", "cap_blocked", "cap_first"] {
        let before = state["guild"]["revision"].as_i64().unwrap();
        let (status, body) = invite(&app, &ot, &mut state, username).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(state["guild"]["revision"], before + 1);
        let mut shape = body.clone();
        shape["state"]["guild"]["revision"] = json!(0);
        shapes.push(shape);
    }
    assert!(shapes.windows(2).all(|w| w[0] == w[1]), "{shapes:?}");
    assert!(pending(&db, &guild, blocker) && pending(&db, &guild, blocked));
    assert!(pending(&db, &guild, first));
    for token in [&bt, &blt] {
        let (status, _) = respond(&app, token, &guild, true).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }
    // A block after inviting hides the invitation and refuses acceptance.
    invite(&app, &ot, &mut state, "cap_second").await;
    safety(&app, &st, "block", owner).await;
    let (status, _) = respond(&app, &st, &guild, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    safety(&app, &st, "unblock", owner).await;

    // Banned accounts cannot be invited, nor accept an earlier invitation.
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"cap_banned"}),
        StatusCode::OK,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"ban_member","account_id":banned}),
        StatusCode::OK,
    )
    .await;
    let (status, _) = invite(&app, &ot, &mut state, "cap_banned").await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    diesel::sql_query("INSERT INTO guild_invitations(guild_id,account_id,invited_by) VALUES($1::uuid,$2::uuid,$3::uuid)")
        .bind::<Text, _>(guild.as_str().unwrap())
        .bind::<Text, _>(banned.to_string())
        .bind::<Text, _>(owner.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let (status, _) = respond(&app, &bant, &guild, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!guild_ids(&app, &bant).await.contains(&guild));

    // Capacity is enforced when accepting: the owner's Free role allows two
    // members; both invitations were sent while there was room.
    diesel::sql_query("UPDATE instance_role_limits SET members=2 WHERE builtin AND role='user'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    let inspect = json!({"action":"inspect","guild_id":guild});
    let current = command(&app, &ot, inspect, StatusCode::OK).await;
    assert_eq!(members(&current["state"]), vec![json!(owner)]);
    let (status, body) = respond(&app, &ft, &guild, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = respond(&app, &st, &guild, true).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "limit_exceeded");
    assert!(pending(&db, &guild, second));
    assert!(!guild_ids(&app, &st).await.contains(&guild));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn invitations_reach_only_the_invitee_live_with_public_fields() {
    let db = database();
    let (owner, ot) = user(&db, "live_owner");
    let (_, it) = user(&db, "live_target");
    let (_, bystander) = user(&db, "live_bystander");
    let (blocker, bt) = user(&db, "live_blocker");
    let (blocked, blt) = user(&db, "live_blocked");
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let (address, server) = serve(&app).await;
    // Blocks in both directions exist before the invitations are sent.
    safety(&app, &bt, "block", owner).await;
    safety(&app, &ot, "block", blocked).await;
    let mut target = open(address, &it).await;
    let mut other = open(address, &bystander).await;
    snapshot(&mut target, |_| true).await;
    snapshot(&mut other, |_| true).await;

    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Live"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    invite(&app, &ot, &mut state, "live_target").await;
    let pushed = snapshot(&mut target, |s| s["guild_invitations"].is_array()).await;
    let invitation = &pushed["guild_invitations"][0];
    assert_eq!(pushed["guild_invitations"].as_array().unwrap().len(), 1);
    assert_eq!(invitation["guild_id"], guild);
    assert_eq!(invitation["guild_name"], "Live");
    assert_eq!(invitation["invited_by"]["id"], json!(owner));
    assert_eq!(invitation["invited_by"]["username"], "live_owner");
    // Only the server name/icon and the inviter's public identity.
    let mut keys: Vec<_> = invitation.as_object().unwrap().keys().cloned().collect();
    keys.sort();
    assert_eq!(keys, ["created_at", "guild_id", "guild_name", "invited_by"]);
    let mut inviter: Vec<_> = invitation["invited_by"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    inviter.sort();
    assert_eq!(inviter, ["display_name", "id", "username"]);
    let parsed: thiscord_shared::permissions::GuildInvitation =
        serde_json::from_value(invitation.clone()).unwrap();
    assert_eq!(parsed.guild_name, "Live");

    // Across a block the invitation is stored but never wakes, lists to or is
    // acceptable by the invitee, in either direction.
    for (account, token, username) in [
        (blocker, &bt, "live_blocker"),
        (blocked, &blt, "live_blocked"),
    ] {
        let mut socket = open(address, token).await;
        snapshot(&mut socket, |_| true).await;
        invite(&app, &ot, &mut state, username).await;
        assert!(pending(&db, &guild, account));
        assert!(quiet(&mut socket).await, "{username} was woken");
        let fresh = snapshot(&mut open(address, token).await, |_| true).await;
        assert!(fresh.get("guild_invitations").is_none(), "{fresh}");
        let (status, _) = respond(&app, token, &guild, true).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
    }

    // Accepting removes it from the invitee's snapshot; others never saw it.
    respond(&app, &it, &guild, true).await;
    snapshot(&mut target, |s| s.get("guild_invitations").is_none()).await;
    let silent = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        snapshot(&mut other, |s| s.get("guild_invitations").is_some()),
    )
    .await;
    assert!(silent.is_err());
    server.abort();
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn members_and_outstanding_invitations_share_the_member_limit_without_an_oracle() {
    let db = database();
    user(&db, "slot_root");
    permissions::bootstrap_owner(&db.pool, "slot_root").unwrap();
    let (owner, ot) = user(&db, "slot_owner");
    let (_, bt) = user(&db, "slot_blocker");
    let (blocked, _) = user(&db, "slot_blocked");
    let (plain, pt) = user(&db, "slot_plain");
    let (late, _) = user(&db, "slot_late");
    user(&db, "slot_member");
    let app = api::router(Some(db.pool.clone()), vec![]);
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Slots"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    // Owner and one accepted member: three slots left under a limit of five.
    member_limit(&db, 5);
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"add_member","username":"slot_member"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(members(&state).len(), 2);
    safety(&app, &bt, "block", owner).await;
    safety(&app, &ot, "block", blocked).await;

    // Delivered and blocked invitations take one slot each with identical responses.
    let mut shapes = vec![];
    for username in ["slot_plain", "slot_blocker", "slot_blocked"] {
        let before = state["guild"]["revision"].as_i64().unwrap();
        let (status, body) = invite(&app, &ot, &mut state, username).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(state["guild"]["revision"], before + 1);
        assert_eq!(members(&state).len(), 2);
        shapes.push(shape(&body));
    }
    assert!(shapes.windows(2).all(|w| w[0] == w[1]), "{shapes:?}");
    // 2 members + 3 invitations = 5: full, although two of them crossed a block.
    let (status, body) = invite(&app, &ot, &mut state, "slot_late").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "limit_exceeded");
    assert!(!pending(&db, &guild, late));
    // At the limit, repeating any outstanding invitation stays an identical success.
    for username in ["slot_plain", "slot_blocker", "slot_blocked"] {
        let (status, body) = invite(&app, &ot, &mut state, username).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(shape(&body), shapes[0]);
    }

    // Declining frees the slot, and only one: the server is full again after it.
    let (status, _) = respond(&app, &pt, &guild, false).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!pending(&db, &guild, plain));
    let (status, body) = invite(&app, &ot, &mut state, "slot_late").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(pending(&db, &guild, late));
    let (status, body) = invite(&app, &ot, &mut state, "slot_plain").await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["code"], "limit_exceeded");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn expired_invitations_free_their_slot_and_cannot_be_listed_or_accepted() {
    let db = database();
    user(&db, "exp_root");
    permissions::bootstrap_owner(&db.pool, "exp_root").unwrap();
    let (_, ot) = user(&db, "exp_owner");
    let (expired, et) = user(&db, "exp_expired");
    let (recent, rt) = user(&db, "exp_recent");
    let (later, _) = user(&db, "exp_later");
    let app = api::router(
        Some(db.pool.clone()),
        vec!["http://localhost:1420".parse().unwrap()],
    );
    let (address, server) = serve(&app).await;
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Expiry"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    member_limit(&db, 3);
    for username in ["exp_expired", "exp_recent"] {
        let (status, body) = invite(&app, &ot, &mut state, username).await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
    let (status, _) = invite(&app, &ot, &mut state, "exp_later").await;
    assert_eq!(status, StatusCode::CONFLICT);

    // Seven days old is expired; six days old is still outstanding.
    age(&db, &guild, expired, "7 days");
    age(&db, &guild, recent, "6 days");
    let listed = snapshot(&mut open(address, &et).await, |_| true).await;
    assert!(listed.get("guild_invitations").is_none(), "{listed}");
    let listed = snapshot(&mut open(address, &rt).await, |_| true).await;
    assert_eq!(listed["guild_invitations"][0]["guild_id"], guild);
    let (status, _) = respond(&app, &et, &guild, true).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert!(!guild_ids(&app, &et).await.contains(&guild));

    // The next invitation to the server deletes it, so its slot is free again.
    assert!(pending(&db, &guild, expired));
    let (status, body) = invite(&app, &ot, &mut state, "exp_later").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!pending(&db, &guild, expired) && pending(&db, &guild, later));
    // Declining an expired (here already deleted) invitation is harmless.
    let (status, body) = respond(&app, &et, &guild, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // An outstanding invitation is unaffected and joins as usual.
    let (status, body) = respond(&app, &rt, &guild, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(pending(&db, &guild, later));
    server.abort();
}
