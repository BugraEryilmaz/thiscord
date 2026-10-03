//! Disposable real-backend benchmark host. See docs/voice-load.md.
#[path = "voice_load/common.rs"]
mod common;

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use common::{Args, Result};
use diesel::{connection::SimpleConnection, prelude::*, sql_types::Text};
use sha2::{Digest, Sha256};
use std::{net::SocketAddr, time::Duration};
use thiscord_backend::{api, db};

struct Fixture {
    admin: PgConnection,
    schema: String,
    pool: Option<db::DbPool>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.pool.take();
        // schema is generated here, never supplied by the caller.
        if self
            .admin
            .batch_execute(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .is_err()
        {
            eprintln!(
                "Cleanup failed; inspect the printed voice_load_* schema in the test database."
            );
        }
    }
}

fn execute(c: &mut PgConnection, sql: &str, args: &[String]) -> QueryResult<usize> {
    let mut query = diesel::sql_query(sql).into_boxed();
    for arg in args {
        query = query.bind::<Text, _>(arg);
    }
    query.execute(c)
}

fn test_database_url() -> Result<String> {
    if let Ok(value) = std::env::var("TEST_DATABASE_URL") {
        return Ok(value);
    }
    // Read only this key, without importing provider, TURN or production settings.
    dotenvy::from_path_iter(concat!(env!("CARGO_MANIFEST_DIR"), "/.env"))
        .ok()
        .and_then(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .find(|(k, _)| k == "TEST_DATABASE_URL")
                .map(|(_, v)| v)
        })
        .ok_or("Set TEST_DATABASE_URL to a dedicated database ending in _test")
}

fn fixture(secret: &str, users: usize, room_size: usize) -> Result<Fixture> {
    let value = test_database_url()?;
    let mut url = validated_test_url(&value)?;
    let mut admin =
        PgConnection::establish(&value).map_err(|_| "Test database connection failed")?;
    admin
        .batch_execute("SET lock_timeout='5s'; SET statement_timeout='10s'")
        .map_err(|_| "Cannot configure fixture connection timeouts")?;
    seed_fixture(&mut url, admin, secret, users, room_size)
}

fn validated_test_url(value: &str) -> Result<url::Url> {
    let url = url::Url::parse(value).map_err(|_| "Invalid TEST_DATABASE_URL")?;
    if !matches!(url.scheme(), "postgres" | "postgresql")
        || !url.path().ends_with("_test")
        || url
            .query_pairs()
            .any(|(k, _)| matches!(k.as_ref(), "options" | "dbname" | "service"))
    {
        return Err(
            "TEST_DATABASE_URL must end in _test, without options/dbname/service overrides",
        );
    }
    Ok(url)
}

fn seed_fixture(
    url: &mut url::Url,
    mut admin: PgConnection,
    secret: &str,
    users: usize,
    room_size: usize,
) -> Result<Fixture> {
    let schema = format!("voice_load_{}", uuid::Uuid::new_v4().simple());
    admin
        .batch_execute(&format!("CREATE SCHEMA {schema}"))
        .map_err(|_| "Cannot create test schema")?;
    let mut fixture = Fixture {
        admin,
        schema,
        pool: None,
    };
    url.query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={}", fixture.schema));
    let pool =
        db::connect_and_migrate(url.as_str()).map_err(|_| "Test schema migrations failed")?;
    fixture.pool = Some(pool.clone());
    let mut c = pool.get().map_err(|_| "Test pool checkout failed")?;
    c.transaction::<_, diesel::result::Error, _>(|c| {
        for index in 0..users {
            let account = common::id("account", index).to_string();
            let session = common::id("session", index).to_string();
            execute(c, "INSERT INTO accounts(id,username,email,display_name,email_verified) VALUES($1::uuid,$2,$2||'@example.test',$2,TRUE)", &[account.clone(), format!("voice_load_{index}")])?;
            execute(c, "INSERT INTO sessions(id,account_id,device) VALUES($1::uuid,$2::uuid,'voice load generator')", &[session.clone(), account])?;
            execute(c, "INSERT INTO session_tokens(token_hash,session_id,active) VALUES($1,$2::uuid,TRUE)", &[URL_SAFE_NO_PAD.encode(Sha256::digest(common::token(secret, index))), session])?;
        }
        let guild = common::id("guild", 0).to_string();
        execute(c, "INSERT INTO guilds(id,name,owner) VALUES($1::uuid,'Disposable voice load',$2::uuid)", &[guild.clone(), common::id("account", 0).to_string()])?;
        execute(c, "INSERT INTO guild_roles(guild_id,name,position,everyone,permissions) VALUES($1::uuid,'Everyone',0,TRUE,'[\"view_channel\",\"join_voice\",\"speak\"]'::jsonb)", std::slice::from_ref(&guild))?;
        for index in 0..users {
            execute(c, "INSERT INTO guild_members(guild_id,account_id) VALUES($1::uuid,$2::uuid)", &[guild.clone(), common::id("account", index).to_string()])?;
        }
        for room in 0..users.div_ceil(room_size) {
            execute(c, "INSERT INTO channels(guild_id,id,name,kind) VALUES($1::uuid,$2::uuid,$3,'voice')", &[guild.clone(), common::id("channel", room).to_string(), format!("load-{room}")])?;
        }
        Ok(())
    }).map_err(|_| "Cannot seed benchmark fixtures")?;
    drop(c);
    Ok(fixture)
}

async fn run() -> Result<()> {
    let mut args = Args::parse()?;
    if args.flag("--help") {
        println!(
            "voice_load_server --bind 0.0.0.0:3001 --users 64 --room-size 8 --allow-insecure\nUsers: 2..500; room size: 2..8. Use --tls instead of --allow-insecure for TLS_CERT_PATH/TLS_KEY_PATH.\nRequires TEST_DATABASE_URL (*_test) and THISCORD_LOAD_SECRET. See docs/voice-load.md."
        );
        return Ok(());
    }
    let bind: SocketAddr = args
        .text("--bind", "127.0.0.1:3001")
        .parse()
        .map_err(|_| "Invalid bind address")?;
    let users = args.number("--users", 64, 2, 500)?;
    let room_size = args.number("--room-size", 8, 2, 8)?;
    let insecure = args.flag("--allow-insecure");
    let tls = args.flag("--tls");
    args.finish()?;
    if tls == insecure {
        return Err("Choose exactly one of --tls or --allow-insecure (trusted LAN only)");
    }
    let secret = common::secret()?;
    let tls = if tls {
        Some(
            thiscord_backend::tls::Tls::from_env()
                .await
                .map_err(|_| "TLS setup failed")?
                .ok_or("TLS paths missing")?,
        )
    } else {
        None
    };
    let fixture = tokio::task::spawn_blocking(move || fixture(&secret, users, room_size))
        .await
        .map_err(|_| "Fixture task failed")??;
    println!("Disposable schema: {}", fixture.schema);
    let app = api::load_test_router(
        fixture.pool.as_ref().unwrap().clone(),
        vec![common::ORIGIN.parse().unwrap()],
        users,
    );
    let serving = tokio::spawn(async move {
        if let Some(tls) = tls {
            axum_server::bind_rustls(bind, tls.config)
                .serve(app.into_make_service())
                .await
                .map_err(|_| "TLS server failed")
        } else {
            let listener = tokio::net::TcpListener::bind(bind)
                .await
                .map_err(|_| "Cannot bind benchmark listener")?;
            axum::serve(listener, app)
                .await
                .map_err(|_| "Benchmark server failed")
        }
    });
    println!(
        "Benchmark host: {bind}; fixture users/connection limit={users}; room size={room_size}. Ctrl+C cleans up."
    );
    let mut serving = serving;
    let result = tokio::select! {
        result = &mut serving => result.unwrap_or(Err("Server task failed")),
        result = tokio::signal::ctrl_c() => result.map_err(|_| "Cannot listen for Ctrl+C"),
    };
    if !serving.is_finished() {
        serving.abort();
        let _ = tokio::time::timeout(Duration::from_secs(2), serving).await;
    }
    tokio::task::spawn_blocking(move || drop(fixture))
        .await
        .map_err(|_| "Cleanup task failed")?;
    result
}

#[tokio::main]
async fn main() {
    if let Err(message) = run().await {
        eprintln!("FAIL: {message}");
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixture_database_cannot_target_application_or_override_search_path() {
        assert!(validated_test_url("postgres://localhost/thiscord").is_err());
        assert!(
            validated_test_url("postgres://localhost/thiscord_test?options=-csearch_path=public")
                .is_err()
        );
        assert!(validated_test_url("postgres://localhost/thiscord_test?dbname=thiscord").is_err());
        assert!(
            validated_test_url("postgres://localhost/thiscord_test?service=production").is_err()
        );
        assert!(validated_test_url("postgres://localhost/thiscord_test?connect_timeout=3").is_ok());
    }
    #[test]
    #[ignore = "requires TEST_DATABASE_URL"]
    fn disposable_fixtures_have_hashed_sessions_and_voice_permissions() {
        let f = fixture(&"a".repeat(64), 16, 8).unwrap();
        let mut c = f.pool.as_ref().unwrap().get().unwrap();
        use thiscord_backend::schema::{accounts, session_tokens};
        assert_eq!(
            accounts::table.count().get_result::<i64>(&mut c).unwrap(),
            16
        );
        assert_eq!(
            session_tokens::table
                .count()
                .get_result::<i64>(&mut c)
                .unwrap(),
            16
        );
        let hash = URL_SAFE_NO_PAD.encode(Sha256::digest(common::token(&"a".repeat(64), 0)));
        assert!(
            session_tokens::table
                .find(hash)
                .select(session_tokens::active)
                .first::<bool>(&mut c)
                .unwrap()
        );
        drop(c);
        let schema = f.schema.clone();
        drop(f);
        let mut c = PgConnection::establish(&test_database_url().unwrap()).unwrap();
        assert_eq!(
            execute(
                &mut c,
                "SELECT 1 FROM pg_namespace WHERE nspname=$1",
                &[schema]
            )
            .unwrap(),
            0
        );
    }
}
