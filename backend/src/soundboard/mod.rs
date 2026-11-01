//! Guild soundboard management and playback downloads. Plays are requested and
//! delivered over voice signaling; see `crate::voice` and docs/soundboard.md.
mod builtin;
mod decode;
mod store;

pub(crate) use store::authorize_play;

use crate::{
    auth::{self, Failure},
    db::DbPool,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, Path, State, rejection::JsonRejection, rejection::PathRejection},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use thiscord_shared::{ChannelId, GuildId, RequestId, SoundId, soundboard::*};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

pub fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(SOUNDBOARD_PATH, post(handle))
        .route(
            &format!("{SOUNDBOARD_PATH}/{{guild_id}}/{{channel_id}}/{{sound_id}}"),
            get(audio),
        )
        .layer(DefaultBodyLimit::max(MAX_UPLOAD_BASE64 + 16 * 1024))
}

fn admission() -> Result<OwnedSemaphorePermit, Failure> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    WORKERS
        .get_or_init(|| Arc::new(Semaphore::new(4)))
        .clone()
        .try_acquire_owned()
        .map_err(|_| Failure::Limited)
}

/// A cold-cache play makes every member of the call download at once. Queue
/// briefly within the playback deadline instead of rejecting the burst.
async fn download_admission() -> Result<OwnedSemaphorePermit, Failure> {
    static DOWNLOADS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    let downloads = DOWNLOADS
        .get_or_init(|| Arc::new(Semaphore::new(4)))
        .clone();
    tokio::time::timeout(Duration::from_secs(2), downloads.acquire_owned())
        .await
        .map_err(|_| Failure::Limited)?
        .map_err(|_| Failure::Unavailable)
}

fn finish(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert("cache-control", "no-store".parse().unwrap());
    headers.insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}

async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<SoundboardRequest>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(request) = body.map_err(|_| Failure::Invalid("Invalid soundboard request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        let permit = admission()?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store::dispatch(&pool, &token, request)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    finish(match result {
        Ok(state) => Json(state).into_response(),
        Err(error) => error.response(id),
    })
}

async fn audio(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    path: Result<Path<(GuildId, ChannelId, SoundId)>, PathRejection>,
) -> Response {
    let result = async {
        let Path((guild, channel, sound)) =
            path.map_err(|_| Failure::Invalid("Invalid sound path"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        let permit = download_admission().await?;
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store::audio(&pool, &token, guild, channel, sound)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    finish(match result {
        Ok(pcm) => ([("content-type", "application/octet-stream")], pcm).into_response(),
        Err(error) => error.response(id),
    })
}
