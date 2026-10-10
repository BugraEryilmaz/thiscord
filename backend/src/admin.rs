//! Explicit instance privilege boundary for operational metadata only.
mod host;
pub mod web;

use crate::{
    auth::{self, Failure, store},
    chat::access,
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
    collections::HashMap,
    future::Future,
    sync::{Arc, OnceLock},
    time::Instant,
};
use thiscord_shared::{ChannelId, RequestId, admin::*, permissions::InstanceRole};
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
    respond(pool, id, start, headers, crate::voice::diagnostics()).await
}

// Collect telemetry without holding DB locks or delivery permits. The final
// scoped epoch check covers revocations committed during this async collection.
async fn respond(
    pool: Option<DbPool>,
    id: RequestId,
    start: Instant,
    headers: HeaderMap,
    active_rooms: impl Future<Output = HashMap<ChannelId, Vec<ParticipantDiagnostics>>>,
) -> Response {
    let result = async {
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = Arc::new(WORKERS.get_or_init(|| Arc::new(Semaphore::new(2))).clone().try_acquire_owned().map_err(|_| Failure::Limited)?);
        let worker_permit = permit.clone();
        let p = pool.clone();
        let (role, names, host, authorized) = tokio::task::spawn_blocking(move || {
            let _permit = worker_permit;
            let mut c = store::connection(&p)?;
            let identity = store::token_session(&mut c, &token)?;
            let account = identity.account_id;
            let authorized = access::snapshot(&access::account(account), Some(&access::instance_role(account)))
                .and_then(|snapshot| snapshot.including(&access::session(identity.id)))
                .ok_or(Failure::Forbidden)?;
            let session = store::authenticate(&mut c, &token)?;
            let result = c.transaction::<_, Failure, _>(|c| {
                store::read_session(c, &token, &session)?;
                let role = crate::permissions::store::instance(c, session.account_id)?.role;
                if !matches!(role, InstanceRole::Owner | InstanceRole::Admin) { return Err(Failure::Forbidden); }
                let names: Vec<RoomDiagnostics> = store::query(c,
                    "SELECT jsonb_build_object('guild_id',g.id,'guild_name',g.name,'channel_id',ch.id,'channel_name',ch.name,'participants','[]'::jsonb) AS data FROM channels ch JOIN guilds g ON g.id=ch.guild_id ORDER BY g.name,ch.name,ch.id", &[])?;
                Ok((role, names))
            })?;
            drop(c);
            Ok::<_, Failure>((result.0, result.1, host::sample(), authorized))
        }).await.map_err(|_| Failure::Unavailable)??;
        let mut active = active_rooms.await;
        let rooms = names.into_iter().filter_map(|mut room| {
            room.participants = active.remove(&room.channel_id)?;
            (!room.participants.is_empty()).then_some(room)
        }).collect();
        let state = pool.state();
        // Serialize while admitted, just as socket delivery does. A logout or
        // demotion must either drain this response or invalidate its snapshot.
        let _delivery = authorized.deliver().ok_or(Failure::Forbidden)?;
        Ok::<_, Failure>(Json(Diagnostics { sampled_at: chrono::Utc::now(), role, version: env!("CARGO_PKG_VERSION").into(), uptime_seconds: start.elapsed().as_secs(), host, database_connections: state.connections, database_idle_connections: state.idle_connections, rooms }).into_response())
    }.await;
    let mut response = match result {
        Ok(response) => response,
        Err(e) => e.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

#[cfg(test)]
mod tests;
