//! Explicit instance privilege boundary for operational metadata only.
mod host;
pub mod web;

use crate::{
    auth::{self, Failure, store},
    db::DbPool,
};
use axum::{
    Extension, Json, Router,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::get,
};
use diesel::Connection;
use std::{
    sync::{Arc, OnceLock},
    time::Instant,
};
use thiscord_shared::{RequestId, admin::*, permissions::InstanceRole};
use tokio::sync::Semaphore;

pub fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(DIAGNOSTICS_PATH, get(handle))
        .layer(Extension(Instant::now()))
}

async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    Extension(start): Extension<Instant>,
    headers: HeaderMap,
) -> Response {
    let result = async {
        // Shares the mutation gate with logout, role changes and ownership transfers.
        let _access = crate::chat::gate().read().await;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = WORKERS.get_or_init(|| Arc::new(Semaphore::new(2))).clone().try_acquire_owned().map_err(|_| Failure::Limited)?;
        let p = pool.clone();
        let (role, names, host) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            let mut c = store::connection(&p)?;
            let session = store::authenticate(&mut c, &token)?;
            let result = c.transaction::<_, Failure, _>(|c| {
                store::lock_session(c, &token, &session)?;
                let role = crate::permissions::store::instance(c, session.account_id)?.role;
                if !matches!(role, InstanceRole::Owner | InstanceRole::Admin) { return Err(Failure::Forbidden); }
                let names: Vec<RoomDiagnostics> = store::query(c,
                    "SELECT jsonb_build_object('guild_id',g.id,'guild_name',g.name,'channel_id',ch.id,'channel_name',ch.name,'participants','[]'::jsonb) AS data FROM channels ch JOIN guilds g ON g.id=ch.guild_id ORDER BY g.name,ch.name,ch.id", &[])?;
                Ok((role, names))
            })?;
            drop(c);
            Ok::<_, Failure>((result.0, result.1, host::sample()))
        }).await.map_err(|_| Failure::Unavailable)??;
        let mut active = crate::voice::diagnostics().await;
        let rooms = names.into_iter().filter_map(|mut room| {
            room.participants = active.remove(&room.channel_id)?;
            (!room.participants.is_empty()).then_some(room)
        }).collect();
        let state = pool.state();
        Ok::<_, Failure>(Diagnostics { sampled_at: chrono::Utc::now(), role, version: env!("CARGO_PKG_VERSION").into(), uptime_seconds: start.elapsed().as_secs(), host, database_connections: state.connections, database_idle_connections: state.idle_connections, rooms })
    }.await;
    let mut response = match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => e.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
