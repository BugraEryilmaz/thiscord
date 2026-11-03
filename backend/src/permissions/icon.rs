//! Guild icons reuse profile-picture normalization. Clients never choose storage
//! paths or URLs; the opaque icon ID changes on every replacement.
use super::{evaluator::effective, store};
use crate::{
    auth::{
        self, Failure,
        store::{self as account, connection, execute},
    },
    db::DbPool,
};
use axum::{
    Extension, Json,
    extract::{Path, State, rejection::JsonRejection, rejection::PathRejection},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use diesel::{
    Connection, PgConnection,
    prelude::*,
    sql_types::{Binary, Uuid as SqlUuid},
};
use thiscord_shared::{AccountId, GuildIconId, GuildId, RequestId, permissions::*};
use uuid::Uuid;

/// Decoding is CPU-bound; keep it off request workers and bounded separately
/// from permission commands so uploads cannot starve guild administration.
fn admit() -> Result<tokio::sync::OwnedSemaphorePermit, Failure> {
    static WORKERS: std::sync::OnceLock<std::sync::Arc<tokio::sync::Semaphore>> =
        std::sync::OnceLock::new();
    WORKERS
        .get_or_init(|| std::sync::Arc::new(tokio::sync::Semaphore::new(2)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| Failure::Limited)
}

pub(super) async fn set(
    State(pool): State<Option<DbPool>>,
    Extension(request_id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<SetGuildIcon>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(command) = body.map_err(|_| Failure::Invalid("Invalid server icon request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        let permit = admit()?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let guild = apply(&pool, &token, command)?;
            // Published after commit; sockets re-check membership before forwarding.
            crate::chat::guild_updated(guild.id);
            Ok::<_, Failure>(guild)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    let mut response = match result {
        Ok(guild) => Json(guild).into_response(),
        Err(error) => error.response(request_id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

/// Runs on a bounded blocking worker. Authentication and rate limiting precede
/// image decoding; decoding precedes the guild lock so it never holds the row.
fn apply(pool: &DbPool, token: &str, command: SetGuildIcon) -> Result<Guild, Failure> {
    let session = account::authenticate(&mut *connection(pool)?, token)?;
    account::rate_limit(
        pool,
        &format!("guild-icon:{}", session.account_id),
        None,
        true,
    )?;
    let png = command
        .image_base64
        .as_deref()
        .map(crate::auth::avatar::normalize)
        .transpose()?;
    let guild_id = command.guild_id;
    connection(pool)?.transaction(|c| {
        account::read_session(c, token, &session)?;
        // Same order as permission commands: session/account -> instance -> guild.
        execute(c, "SELECT singleton FROM instance FOR SHARE", &[])?;
        execute(
            c,
            "SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE",
            &[&guild_id.to_string()],
        )?;
        let state = store::load(c, guild_id)?;
        authorize(&state, session.account_id, command.revision)?;
        if write(c, guild_id, png)? {
            execute(
                c,
                "UPDATE guilds SET revision=revision+1 WHERE id=$1::uuid",
                &[&guild_id.to_string()],
            )?;
        }
        Ok(store::load(c, guild_id)?.guild)
    })
}

fn authorize(state: &GuildState, actor: AccountId, revision: i64) -> Result<(), Failure> {
    // Non-members (including instance admins) are indistinguishable from a missing guild.
    if !state.members.iter().any(|m| m.account_id == actor) {
        return Err(Failure::Forbidden);
    }
    if !effective(state, actor, None).contains(&Permission::ManageGuild) {
        return Err(Failure::Forbidden);
    }
    if state.guild.revision != revision {
        return Err(Failure::Conflict);
    }
    Ok(())
}

/// Returns whether the stored icon changed.
fn write(c: &mut PgConnection, guild: GuildId, png: Option<Vec<u8>>) -> Result<bool, Failure> {
    Ok(match png {
        Some(png) => {
            diesel::sql_query("INSERT INTO guild_icons(guild_id,id,png) VALUES($1,$2,$3) ON CONFLICT(guild_id) DO UPDATE SET id=EXCLUDED.id,png=EXCLUDED.png")
                .bind::<SqlUuid, _>(guild.as_uuid())
                .bind::<SqlUuid, _>(Uuid::new_v4())
                .bind::<Binary, _>(png)
                .execute(c)?;
            true
        }
        None => {
            execute(
                c,
                "DELETE FROM guild_icons WHERE guild_id=$1::uuid",
                &[&guild.to_string()],
            )? > 0
        }
    })
}

/// Presentation media at an opaque, replaceable UUID that only members are told.
/// Like profile pictures, it is served without a bearer token so `<img>` and the
/// desktop WebView can load it; there is no guild-ID lookup or listing.
pub(super) async fn get(
    State(pool): State<Option<DbPool>>,
    Extension(request_id): Extension<RequestId>,
    path: Result<Path<GuildIconId>, PathRejection>,
) -> Response {
    let result = async {
        let Path(id) = path.map_err(|_| Failure::Invalid("Invalid server icon ID"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        tokio::task::spawn_blocking(move || {
            #[derive(QueryableByName)]
            struct Icon {
                #[diesel(sql_type = Binary)]
                png: Vec<u8>,
            }
            let mut c = connection(&pool)?;
            let icon = diesel::sql_query("SELECT png FROM guild_icons WHERE id=$1")
                .bind::<SqlUuid, _>(id.as_uuid())
                .get_result::<Icon>(&mut c)
                .optional()?
                .ok_or(Failure::NotFound)?;
            Ok::<_, Failure>(icon.png)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    match result {
        Ok(png) => (
            [
                ("content-type", "image/png"),
                ("x-content-type-options", "nosniff"),
                ("cache-control", "private, max-age=300"),
            ],
            png,
        )
            .into_response(),
        Err(error) => {
            let mut response = error.response(request_id);
            response
                .headers_mut()
                .insert("cache-control", "no-store".parse().unwrap());
            response
        }
    }
}
