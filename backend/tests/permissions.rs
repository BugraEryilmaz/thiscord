use axum::{
    Router,
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use diesel::{connection::SimpleConnection, prelude::*, sql_types::Text};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiscord_backend::{api, db, permissions};
use thiscord_shared::{AccountId, ChannelId, GuildId, RoleId, permissions::*};
use tower::ServiceExt;
use uuid::Uuid;

struct Database {
    connection: PgConnection,
    schema: String,
    pool: db::DbPool,
}
impl Drop for Database {
    fn drop(&mut self) {
        self.connection
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .unwrap();
    }
}
fn database() -> Database {
    dotenvy::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/.env")).ok();
    let value = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let mut url = url::Url::parse(&value).unwrap();
    assert!(url.path().ends_with("_test"));
    let mut connection = PgConnection::establish(&value).unwrap();
    let schema = format!("permissions_test_{}", Uuid::new_v4().simple());
    connection
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .unwrap();
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = db::connect_and_migrate(url.as_str()).unwrap();
    Database {
        connection,
        schema,
        pool,
    }
}
fn user(db: &Database, name: &str) -> (AccountId, String) {
    let id = AccountId::from_uuid(Uuid::new_v4());
    let token = URL_SAFE_NO_PAD.encode(Sha256::digest(Uuid::new_v4().as_bytes()));
    let sid = Uuid::new_v4();
    let mut c = db.pool.get().unwrap();
    diesel::sql_query("INSERT INTO accounts(id,username,email,display_name,email_verified) VALUES($1::uuid,$2,$2||'@example.test',$2,TRUE)").bind::<Text,_>(id.to_string()).bind::<Text,_>(name).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO identities(account_id,provider,subject,password_hash) VALUES($1::uuid,'password',$1,'test-unused-hash')").bind::<Text,_>(id.to_string()).execute(&mut c).unwrap();
    diesel::sql_query("INSERT INTO sessions(id,account_id,device,reauthenticated_at) VALUES($1::uuid,$2::uuid,'test',now())").bind::<Text,_>(sid.to_string()).bind::<Text,_>(id.to_string()).execute(&mut c).unwrap();
    diesel::sql_query(
        "INSERT INTO session_tokens(token_hash,session_id,active) VALUES($1,$2::uuid,TRUE)",
    )
    .bind::<Text, _>(URL_SAFE_NO_PAD.encode(Sha256::digest(token.as_bytes())))
    .bind::<Text, _>(sid.to_string())
    .execute(&mut c)
    .unwrap();
    (id, token)
}
async fn call(app: &Router, path: &str, token: &str, command: Value) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri(path)
                .header("content-type", "application/json")
                .header("authorization", format!("Bearer {token}"))
                .body(Body::from(command.to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    assert_eq!(response.headers()["cache-control"], "no-store");
    let request_id = response.headers()["x-request-id"]
        .to_str()
        .unwrap()
        .to_owned();
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 2_000_000).await.unwrap()).unwrap();
    if !status.is_success() {
        assert_eq!(body["request_id"], request_id);
    }
    (status, body)
}
async fn command(app: &Router, token: &str, command: Value, expected: StatusCode) -> Value {
    let (status, body) = call(app, PERMISSIONS_PATH, token, command).await;
    assert_eq!(status, expected, "{body}");
    body
}
async fn change(
    app: &Router,
    token: &str,
    state: &mut Value,
    change: Value,
    expected: StatusCode,
) -> Value {
    let body=command(app,token,json!({"action":"change","guild_id":state["guild"]["id"],"revision":state["guild"]["revision"],"change":change}),expected).await;
    if body["result"] == "state" {
        *state = body["state"].clone();
    }
    body
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn ownership_hierarchy_isolation_stale_access_and_concurrency() {
    let db = database();
    let (owner, ot) = user(&db, "owner");
    let (manager, mt) = user(&db, "manager");
    let (member, ut) = user(&db, "member");
    let (_, outsider) = user(&db, "outsider");
    let app = api::router(Some(db.pool.clone()), vec![]);
    command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Alpha"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let before = command(&app, &ot, json!({"action":"instance"}), StatusCode::OK).await;
    assert_eq!(before["access"]["role"], "user");
    let p1 = db.pool.clone();
    let p2 = db.pool.clone();
    let (a, b) = tokio::join!(
        tokio::task::spawn_blocking(move || permissions::bootstrap_owner(&p1, "owner")),
        tokio::task::spawn_blocking(move || permissions::bootstrap_owner(&p2, "owner"))
    );
    assert_eq!(
        usize::from(a.unwrap().is_ok()) + usize::from(b.unwrap().is_ok()),
        1
    );
    let mut state = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Alpha"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let other = command(
        &app,
        &ot,
        json!({"action":"create_guild","name":"Beta"}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    let guild = state["guild"]["id"].clone();
    command(
        &app,
        &outsider,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        "",
        json!({"action":"list_guilds"}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    for username in ["manager", "member"] {
        change(
            &app,
            &ot,
            &mut state,
            json!({"action":"add_member","username":username}),
            StatusCode::OK,
        )
        .await;
    }
    change(&app,&ot,&mut state,json!({"action":"create_role","name":"Manager","position":50,"permissions":["manage_guild","manage_roles","manage_channels","kick_members"]}),StatusCode::OK).await;
    let manager_role = state["roles"][0]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"assign_role","account_id":manager,"role_id":manager_role,"assigned":true}),
        StatusCode::OK,
    )
    .await;
    change(&app,&mt,&mut state,json!({"action":"create_role","name":"Escalate","position":10,"permissions":["administrator"]}),StatusCode::FORBIDDEN).await;
    change(
        &app,
        &mt,
        &mut state,
        json!({"action":"create_role","name":"Equal","position":50,"permissions":[]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(&app,&mt,&mut state,json!({"action":"assign_role","account_id":manager,"role_id":manager_role,"assigned":false}),StatusCode::FORBIDDEN).await;
    change(
        &app,
        &mt,
        &mut state,
        json!({"action":"remove_member","account_id":owner}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let foreign_role = other["roles"][0]["id"].clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"assign_role","account_id":member,"role_id":foreign_role,"assigned":true}),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(&app,&mt,&mut state,json!({"action":"create_role","name":"Helper","position":10,"permissions":["send_messages"]}),StatusCode::OK).await;
    let helper = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["name"] == "Helper")
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &mt,
        &mut state,
        json!({"action":"assign_role","account_id":member,"role_id":helper,"assigned":true}),
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
    let everyone = state["roles"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["everyone"] == true)
        .unwrap()["id"]
        .clone();
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"delete_role","role_id":everyone}),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"role","id":everyone},"allow":[],"deny":["view_channel"]}),StatusCode::OK).await;
    let preview = command(
        &app,
        &ut,
        json!({"action":"preview","guild_id":guild,"account_id":member,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    assert!(
        !preview["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("send_messages"))
    );
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"role","id":helper},"allow":["view_channel"],"deny":[]}),StatusCode::OK).await;
    let preview = command(
        &app,
        &ut,
        json!({"action":"preview","guild_id":guild,"account_id":member,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    assert!(
        preview["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("send_messages"))
    );
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":[],"deny":["send_messages"]}),StatusCode::OK).await;
    let preview = command(
        &app,
        &ut,
        json!({"action":"preview","guild_id":guild,"account_id":member,"channel_id":channel}),
        StatusCode::OK,
    )
    .await;
    assert!(
        !preview["permissions"]
            .as_array()
            .unwrap()
            .contains(&json!("send_messages"))
    );
    change(&app,&ot,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":member},"allow":["send_messages"],"deny":["send_messages"]}),StatusCode::FORBIDDEN).await;
    // Both writers start from the same revision; exactly one may commit.
    let edit = json!({"action":"change","guild_id":guild,"revision":state["guild"]["revision"],"change":{"action":"rename","name":"Concurrent"}});
    let (a, b) = tokio::join!(
        call(&app, PERMISSIONS_PATH, &ot, edit.clone()),
        call(&app, PERMISSIONS_PATH, &mt, edit)
    );
    assert!(
        (a.0 == StatusCode::OK && b.0 == StatusCode::CONFLICT)
            || (b.0 == StatusCode::OK && a.0 == StatusCode::CONFLICT)
    );
    state = command(
        &app,
        &ot,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    change(&app,&ot,&mut state,json!({"action":"assign_role","account_id":manager,"role_id":manager_role,"assigned":false}),StatusCode::OK).await;
    command(
        &app,
        &mt,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(
        &app,
        &mt,
        &mut state,
        json!({"action":"create_role","name":"Stale","position":1,"permissions":[]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"remove_member","account_id":member}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        &ut,
        json!({"action":"preview","guild_id":guild,"account_id":member,"channel_id":channel}),
        StatusCode::FORBIDDEN,
    )
    .await;
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"leave"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let (status, _) = call(
        &app,
        thiscord_shared::account::ACCOUNT_PATH,
        &ot,
        json!({"action":"delete_account","confirmation":"owner"}),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    change(
        &app,
        &ot,
        &mut state,
        json!({"action":"transfer_owner","account_id":manager}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        &mt,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        &ot,
        json!({"action":"transfer_instance","account_id":manager}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        &ot,
        json!({"action":"set_instance_admin","account_id":member,"admin":true}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        &mt,
        json!({"action":"set_instance_admin","account_id":member,"admin":true}),
        StatusCode::OK,
    )
    .await;
    command(
        &app,
        &ut,
        json!({"action":"inspect","guild_id":other["guild"]["id"]}),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert!(permissions::bootstrap_owner(&db.pool, "owner").is_err());
    // Transferring/deleting owned resources makes ordinary account deletion possible.
    let mut other = other;
    change(
        &app,
        &ot,
        &mut other,
        json!({"action":"delete"}),
        StatusCode::OK,
    )
    .await;
    let previous = command(
        &app,
        &mt,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    let (status, _) = call(
        &app,
        thiscord_shared::account::ACCOUNT_PATH,
        &ot,
        json!({"action":"delete_account","confirmation":"owner"}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let remaining = command(
        &app,
        &mt,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(
        remaining["state"]["guild"]["revision"].as_i64().unwrap(),
        previous["state"]["guild"]["revision"].as_i64().unwrap() + 1
    );
    assert!(
        !remaining["state"]["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["account_id"] == json!(owner))
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn bootstrap_verification_reauthentication_and_revoked_sessions() {
    let db = database();
    let (owner, token) = user(&db, "owner");
    let (other, other_token) = user(&db, "other");
    let app = api::router(Some(db.pool.clone()), vec![]);
    diesel::sql_query("UPDATE accounts SET email_verified=FALSE WHERE id=$1::uuid")
        .bind::<Text, _>(owner.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    assert!(permissions::bootstrap_owner(&db.pool, "owner").is_err());
    diesel::sql_query("UPDATE accounts SET email_verified=TRUE WHERE id=$1::uuid")
        .bind::<Text, _>(owner.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    permissions::bootstrap_owner(&db.pool, "owner").unwrap();
    diesel::sql_query("UPDATE sessions SET reauthenticated_at=NULL WHERE account_id=$1::uuid")
        .bind::<Text, _>(owner.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    command(
        &app,
        &token,
        json!({"action":"transfer_instance","account_id":other}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        &token,
        json!({"action":"set_instance_admin","account_id":other,"admin":true}),
        StatusCode::FORBIDDEN,
    )
    .await;
    assert!(
        diesel::sql_query("DELETE FROM accounts WHERE id=$1::uuid")
            .bind::<Text, _>(owner.to_string())
            .execute(&mut db.pool.get().unwrap())
            .is_err()
    );
    command(
        &app,
        &token,
        json!({"action":"instance","grant":"owner"}),
        StatusCode::BAD_REQUEST,
    )
    .await;
    diesel::sql_query("UPDATE sessions SET revoked=TRUE WHERE account_id=$1::uuid")
        .bind::<Text, _>(other.to_string())
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    command(
        &app,
        &other_token,
        json!({"action":"list_guilds"}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
    diesel::sql_query(
        "UPDATE sessions SET expires_at=now()-interval '1 second' WHERE account_id=$1::uuid",
    )
    .bind::<Text, _>(owner.to_string())
    .execute(&mut db.pool.get().unwrap())
    .unwrap();
    command(
        &app,
        &token,
        json!({"action":"instance"}),
        StatusCode::UNAUTHORIZED,
    )
    .await;
}

#[test]
fn combined_roles_overrides_and_admin_behavior() {
    let owner = AccountId::from_uuid(Uuid::new_v4());
    let member = AccountId::from_uuid(Uuid::new_v4());
    let outsider = AccountId::from_uuid(Uuid::new_v4());
    let role = |position, permissions| Role {
        id: RoleId::from_uuid(Uuid::new_v4()),
        name: "role".into(),
        position,
        everyone: position == 0,
        permissions,
    };
    let everyone = role(
        0,
        [
            Permission::ViewChannel,
            Permission::JoinVoice,
            Permission::Speak,
        ]
        .into_iter()
        .collect(),
    );
    let a = role(1, [Permission::SendMessages].into_iter().collect());
    let b = role(1, Permissions::new());
    let channel = ChannelId::from_uuid(Uuid::new_v4());
    let mut state = GuildState {
        guild: Guild {
            id: GuildId::from_uuid(Uuid::new_v4()),
            owner,
            name: "guild".into(),
            revision: 0,
        },
        members: vec![
            Member {
                account_id: owner,
                username: "owner".into(),
                roles: vec![],
            },
            Member {
                account_id: member,
                username: "member".into(),
                roles: vec![a.id, b.id],
            },
        ],
        channels: vec![Channel {
            id: channel,
            name: "channel".into(),
            kind: ChannelKind::Voice,
        }],
        overrides: vec![
            ChannelOverride {
                channel_id: channel,
                target: OverrideTarget::Role(a.id),
                allow: Permissions::new(),
                deny: [Permission::SendMessages, Permission::JoinVoice]
                    .into_iter()
                    .collect(),
            },
            ChannelOverride {
                channel_id: channel,
                target: OverrideTarget::Role(b.id),
                allow: [Permission::SendMessages].into_iter().collect(),
                deny: Permissions::new(),
            },
        ],
        roles: vec![everyone, a, b],
    };
    let p = permissions::evaluator::effective(&state, member, Some(channel));
    assert!(p.contains(&Permission::SendMessages));
    assert!(!p.contains(&Permission::Speak));
    state.overrides.reverse();
    assert_eq!(
        p,
        permissions::evaluator::effective(&state, member, Some(channel))
    );
    assert!(permissions::evaluator::effective(&state, outsider, Some(channel)).is_empty());
    assert_eq!(
        permissions::evaluator::effective(&state, owner, Some(channel)).len(),
        Permission::ALL.len()
    );
    state.roles[1].permissions.insert(Permission::Administrator);
    assert_eq!(
        permissions::evaluator::effective(&state, member, Some(channel)).len(),
        Permission::ALL.len()
    );
    assert!(
        permissions::evaluator::effective(
            &state,
            member,
            Some(ChannelId::from_uuid(Uuid::new_v4()))
        )
        .is_empty()
    );
}

#[tokio::test]
#[ignore = "requires TEST_DATABASE_URL; CI runs with --include-ignored"]
async fn password_protected_join_and_member_channel_visibility() {
    let db = database();
    let (_, owner) = user(&db, "owner");
    let (account, guest) = user(&db, "guest");
    let (_, unverified) = user(&db, "unverified");
    permissions::bootstrap_owner(&db.pool, "owner").unwrap();
    let app = api::router(Some(db.pool.clone()), vec![]);
    command(
        &app,
        &guest,
        json!({"action":"create_guild","name":"Denied","password":"secret"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let response = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Protected","password":"server secret"}),
        StatusCode::OK,
    )
    .await;
    assert!(!response.to_string().contains("password_hash"));
    assert!(!response.to_string().contains("server secret"));
    let mut state = response["state"].clone();
    let guild = state["guild"]["id"].clone();
    #[derive(QueryableByName)]
    struct Hash {
        #[diesel(sql_type=Text)]
        hash: String,
    }
    let hash = diesel::sql_query("SELECT password_hash AS hash FROM guilds WHERE id=$1::uuid")
        .bind::<Text, _>(guild.as_str().unwrap())
        .get_result::<Hash>(&mut db.pool.get().unwrap())
        .unwrap()
        .hash;
    assert!(hash.starts_with("$argon2id$"));
    assert!(!hash.contains("server secret"));
    command(
        &app,
        &guest,
        json!({"action":"view_guild","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        &guest,
        json!({"action":"join_guild","guild_id":guild}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        &guest,
        json!({"action":"join_guild","guild_id":guild,"password":"wrong"}),
        StatusCode::FORBIDDEN,
    )
    .await;
    let joined = command(
        &app,
        &guest,
        json!({"action":"join_guild","guild_id":guild,"password":"server secret"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(joined["result"], "joined");
    assert!(!joined.to_string().contains("password"));
    let again = command(
        &app,
        &guest,
        json!({"action":"join_guild","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(joined["guild"]["revision"], again["guild"]["revision"]);
    state = command(
        &app,
        &owner,
        json!({"action":"inspect","guild_id":guild}),
        StatusCode::OK,
    )
    .await["state"]
        .clone();
    assert_eq!(state["members"].as_array().unwrap().len(), 2);
    change(
        &app,
        &owner,
        &mut state,
        json!({"action":"create_channel","name":"hidden","kind":"text"}),
        StatusCode::OK,
    )
    .await;
    let channel = state["channels"][0]["id"].clone();
    change(&app,&owner,&mut state,json!({"action":"set_override","channel_id":channel,"target":{"kind":"member","id":account},"allow":[],"deny":["view_channel"]}),StatusCode::OK).await;
    let home = command(
        &app,
        &guest,
        json!({"action":"view_guild","guild_id":guild}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(home["home"]["channels"], json!([]));
    assert_eq!(home["home"]["can_manage_roles"], false);
    assert!(!home.to_string().contains("password"));
    let open = command(
        &app,
        &owner,
        json!({"action":"create_guild","name":"Open","password":""}),
        StatusCode::OK,
    )
    .await["state"]["guild"]["id"]
        .clone();
    command(
        &app,
        &guest,
        json!({"action":"join_guild","guild_id":open}),
        StatusCode::OK,
    )
    .await;
    let listed = command(
        &app,
        &guest,
        json!({"action":"list_guilds"}),
        StatusCode::OK,
    )
    .await;
    assert_eq!(listed["guilds"].as_array().unwrap().len(), 2);
    assert!(!listed.to_string().contains("password"));
    command(
        &app,
        &guest,
        json!({"action":"join_guild","guild_id":Uuid::new_v4()}),
        StatusCode::FORBIDDEN,
    )
    .await;
    command(
        &app,
        &guest,
        json!({"action":"join_guild","guild_id":open}),
        StatusCode::TOO_MANY_REQUESTS,
    )
    .await;
    diesel::sql_query("UPDATE accounts SET email_verified=FALSE WHERE username='unverified'")
        .execute(&mut db.pool.get().unwrap())
        .unwrap();
    command(
        &app,
        &unverified,
        json!({"action":"join_guild","guild_id":open}),
        StatusCode::FORBIDDEN,
    )
    .await;
}
