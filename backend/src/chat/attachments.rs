//! Bounded personal-deployment bytea storage. Never trust the supplied MIME hint.
use super::store;
use crate::{
    auth::{
        self, Failure,
        store::{connection, execute, query},
    },
    db::DbPool,
};
use axum::{
    Extension, Json, Router,
    body::{Body, Bytes, to_bytes},
    extract::{
        Path, Query, Request, State,
        rejection::{PathRejection, QueryRejection},
    },
    response::{IntoResponse, Response},
    routing::{get, post},
};
use diesel::{
    prelude::*,
    sql_types::{Binary, Text, Uuid as SqlUuid},
};
use std::{
    sync::{Arc, Mutex, OnceLock},
    time::Duration,
};
use thiscord_shared::{
    AccountId, AttachmentId, ChannelId, GuildId, MessageId, RequestId, chat::*,
    permissions::Permission,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

pub(super) fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(ATTACHMENTS_PATH, post(upload))
        .route("/api/v1/attachments/{id}", get(download))
}

fn admission() -> Result<Arc<OwnedSemaphorePermit>, Failure> {
    static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    WORKERS
        .get_or_init(|| Arc::new(Semaphore::new(4)))
        .clone()
        .try_acquire_owned()
        .map(Arc::new)
        .map_err(|_| Failure::Limited)
}

fn response(result: Result<Response, Failure>, id: RequestId) -> Response {
    let mut response = result.unwrap_or_else(|error| error.response(id));
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}

fn validate_upload(upload: &AttachmentUpload) -> Result<(), Failure> {
    if upload.filename.trim().is_empty()
        || upload.filename.len() > MAX_ATTACHMENT_FILENAME_BYTES
        || upload
            .filename
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        || matches!(upload.filename.as_str(), "." | "..")
        || upload.content_type.len() > 127
        || upload.content_type.chars().any(char::is_control)
    {
        return Err(Failure::Invalid(
            "Invalid attachment filename or content type",
        ));
    }
    Ok(())
}

async fn upload(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    query: Result<Query<AttachmentUpload>, QueryRejection>,
    request: Request,
) -> Response {
    let result = async {
        // Request retains the unread body until bounded admission and authorization succeed.
        let permit = admission()?;
        let Query(upload) = query.map_err(|_| Failure::Invalid("Invalid attachment query"))?;
        validate_upload(&upload)?;
        if let Some(length) = request.headers().get("content-length") {
            let length = length.to_str().ok().and_then(|v| v.parse::<usize>().ok())
                .ok_or(Failure::Invalid("Invalid attachment length"))?;
            if length == 0 || length > MAX_ATTACHMENT_BYTES {
                return Err(Failure::Invalid("Attachment must contain 1 byte to 25 MiB"));
            }
        }
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(request.headers());
        let (p, t, u, guard) = (pool.clone(), token.clone(), upload.clone(), permit.clone());
        tokio::task::spawn_blocking(move || {
            let _guard = guard;
            store::checked(&p, &t, u.guild_id, Some(u.channel_id), Some(Permission::SendMessages),
                |c, actor, _| quota(c, actor, 0))
        }).await.map_err(|_| Failure::Unavailable)??;
        let bytes = tokio::time::timeout(Duration::from_secs(30), to_bytes(request.into_body(), MAX_ATTACHMENT_BYTES))
            .await.map_err(|_| Failure::Invalid("Attachment upload timed out"))?
            .map_err(|_| Failure::Invalid("Invalid attachment body; maximum is 25 MiB"))?;
        if bytes.is_empty() { return Err(Failure::Invalid("Attachment is empty")); }
        let metadata = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            store::checked(&pool, &token, upload.guild_id, Some(upload.channel_id), Some(Permission::SendMessages), |c, actor, _| {
                // Serialize account-wide quota checks across channels without upgrading account locks.
                execute(c, "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))", &[&format!("attachment:{actor}")])?;
                cleanup(c)?;
                quota(c, actor, bytes.len())?;
                let attachment = ChatAttachment { id: AttachmentId::from_uuid(Uuid::new_v4()),
                    filename: upload.filename, content_type: classify(&bytes).into(), size_bytes: bytes.len() as u64 };
                diesel::sql_query("INSERT INTO attachments(id,guild_id,channel_id,owner_id,filename,content_type,size_bytes,data) VALUES($1,$2,$3,$4,$5,$6,octet_length($7),$7)")
                    .bind::<SqlUuid,_>(attachment.id.as_uuid()).bind::<SqlUuid,_>(upload.guild_id.as_uuid())
                    .bind::<SqlUuid,_>(upload.channel_id.as_uuid()).bind::<SqlUuid,_>(actor.as_uuid())
                    .bind::<Text,_>(&attachment.filename).bind::<Text,_>(&attachment.content_type)
                    .bind::<Binary,_>(bytes.as_ref()).execute(c)?;
                Ok(attachment)
            })
        }).await.map_err(|_| Failure::Unavailable)??;
        Ok(Json(metadata).into_response())
    }.await;
    response(result, id)
}

fn quota(c: &mut PgConnection, actor: AccountId, incoming: usize) -> Result<(), Failure> {
    let totals: Vec<serde_json::Value> = query(
        c,
        "SELECT jsonb_build_object('count',count(*),'bytes',COALESCE(sum(size_bytes),0)) AS data FROM attachments WHERE owner_id=$1::uuid AND message_id IS NULL AND expires_at>now()",
        &[&actor.to_string()],
    )?;
    let total = totals.first().ok_or(Failure::Unavailable)?;
    if total["count"].as_u64().unwrap_or(u64::MAX) >= MAX_STAGED_ATTACHMENTS as u64
        || total["bytes"]
            .as_u64()
            .unwrap_or(u64::MAX)
            .saturating_add(incoming as u64)
            > MAX_STAGED_ATTACHMENT_BYTES as u64
    {
        return Err(Failure::Limited);
    }
    Ok(())
}

pub(super) fn validate_ids(ids: &[AttachmentId]) -> Result<(), Failure> {
    if ids.len() > MAX_MESSAGE_ATTACHMENTS
        || ids.iter().collect::<std::collections::BTreeSet<_>>().len() != ids.len()
    {
        return Err(Failure::Invalid("Choose at most four distinct attachments"));
    }
    Ok(())
}

pub(super) fn attach(
    c: &mut PgConnection,
    actor: AccountId,
    guild: GuildId,
    channel: ChannelId,
    message: MessageId,
    ids: &[AttachmentId],
) -> Result<(), Failure> {
    let mut metadata = Vec::<ChatAttachment>::new();
    for id in ids {
        let mut rows = query(
            c,
            "UPDATE attachments SET message_id=$1::uuid WHERE id=$2::uuid AND owner_id=$3::uuid AND guild_id=$4::uuid AND channel_id=$5::uuid AND message_id IS NULL AND expires_at>now() RETURNING jsonb_build_object('id',id,'filename',filename,'content_type',content_type,'size_bytes',size_bytes) AS data",
            &[
                &message.to_string(),
                &id.to_string(),
                &actor.to_string(),
                &guild.to_string(),
                &channel.to_string(),
            ],
        )?;
        metadata.push(rows.pop().ok_or(Failure::Forbidden)?);
    }
    if !metadata.is_empty() {
        execute(
            c,
            "UPDATE messages SET attachments=$2::jsonb WHERE id=$1::uuid",
            &[
                &message.to_string(),
                &serde_json::to_string(&metadata).map_err(|_| Failure::Unavailable)?,
            ],
        )?;
    }
    Ok(())
}

#[derive(QueryableByName)]
struct Download {
    #[diesel(sql_type = Binary)]
    data: Vec<u8>,
    #[diesel(sql_type = Text)]
    filename: String,
    #[diesel(sql_type = Text)]
    content_type: String,
}
async fn download(
    State(pool): State<Option<DbPool>>,
    Extension(request_id): Extension<RequestId>,
    path: Result<Path<AttachmentId>, PathRejection>,
    headers: axum::http::HeaderMap,
) -> Response {
    let result = async {
        let permit = admission()?;
        let Path(id) = path.map_err(|_| Failure::Invalid("Invalid attachment ID"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        let worker_permit = permit.clone();
        let file = tokio::task::spawn_blocking(move || {
            let _permit = worker_permit;
            let mut c = connection(&pool)?;
            auth::store::authenticate(&mut c, &token)?;
            let scope: Vec<(GuildId, ChannelId)> = query(&mut c,
                "SELECT jsonb_build_array(guild_id,channel_id) AS data FROM attachments WHERE id=$1::uuid AND message_id IS NOT NULL", &[&id.to_string()])?;
            let (guild, channel) = scope.first().copied().ok_or(Failure::Forbidden)?;
            drop(c);
            store::checked(&pool, &token, guild, Some(channel), Some(Permission::ReadHistory), |c, _, _| {
                execute(c, "SELECT id FROM channels WHERE guild_id=$1::uuid AND id=$2::uuid FOR SHARE", &[&guild.to_string(), &channel.to_string()])?;
                diesel::sql_query("SELECT a.data,a.filename,a.content_type FROM attachments a JOIN messages m ON m.id=a.message_id AND m.guild_id=a.guild_id AND m.channel_id=a.channel_id WHERE a.id=$1 AND a.guild_id=$2 AND a.channel_id=$3 AND NOT m.deleted")
                    .bind::<SqlUuid,_>(id.as_uuid()).bind::<SqlUuid,_>(guild.as_uuid()).bind::<SqlUuid,_>(channel.as_uuid())
                    .get_result::<Download>(c).optional()?.ok_or(Failure::Forbidden)
            })
        }).await.map_err(|_| Failure::Unavailable)??;
        let disposition = disposition(&file.filename);
        let length = file.data.len().to_string();
        let body = Body::from_stream(DownloadStream::new(file.data, permit, Duration::from_secs(60)));
        Ok(([ ("content-type", file.content_type), ("content-disposition", disposition), ("content-length", length) ], body).into_response())
    }.await;
    response(result, request_id)
}

struct Transfer {
    bytes: Vec<u8>,
    position: usize,
    _permit: Arc<OwnedSemaphorePermit>,
}

/// The deadline task can release the large buffer even when a blocked transport
/// stops polling the body. Each emitted chunk owns only its bounded copy.
struct DownloadStream {
    transfer: Arc<Mutex<Option<Transfer>>>,
    deadline: tokio::task::AbortHandle,
    done: bool,
}
impl DownloadStream {
    fn new(bytes: Vec<u8>, permit: Arc<OwnedSemaphorePermit>, timeout: Duration) -> Self {
        let transfer = Arc::new(Mutex::new(Some(Transfer {
            bytes,
            position: 0,
            _permit: permit,
        })));
        let expired = transfer.clone();
        let deadline = tokio::spawn(async move {
            tokio::time::sleep(timeout).await;
            expired.lock().unwrap().take();
        })
        .abort_handle();
        Self {
            transfer,
            deadline,
            done: false,
        }
    }
}
impl futures_util::Stream for DownloadStream {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        use std::task::Poll;
        if self.done {
            return Poll::Ready(None);
        }
        let mut guard = self.transfer.lock().unwrap();
        let Some(transfer) = guard.as_mut() else {
            drop(guard);
            self.done = true;
            return Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Attachment transfer expired",
            ))));
        };
        if transfer.position == transfer.bytes.len() {
            guard.take();
            drop(guard);
            self.deadline.abort();
            self.done = true;
            return Poll::Ready(None);
        }
        let end = (transfer.position + 64 * 1024).min(transfer.bytes.len());
        let bytes = Bytes::copy_from_slice(&transfer.bytes[transfer.position..end]);
        transfer.position = end;
        Poll::Ready(Some(Ok(bytes)))
    }
}
impl Drop for DownloadStream {
    fn drop(&mut self) {
        self.deadline.abort();
        self.transfer.lock().unwrap().take();
    }
}

fn disposition(filename: &str) -> String {
    use std::fmt::Write;
    let mut value = String::from("attachment; filename=\"download\"; filename*=UTF-8''");
    for byte in filename.bytes() {
        write!(&mut value, "%{byte:02X}").unwrap();
    }
    value
}

fn classify(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        "image/webp"
    } else if bytes.get(4..8) == Some(b"ftyp")
        && matches!(
            bytes.get(8..12),
            Some(b"isom" | b"iso2" | b"mp41" | b"mp42" | b"avc1")
        )
    {
        "video/mp4"
    } else if bytes.starts_with(b"\x1a\x45\xdf\xa3")
        && bytes[..bytes.len().min(128)]
            .windows(4)
            .any(|w| w == b"webm")
    {
        "video/webm"
    } else {
        "application/octet-stream"
    }
}

fn cleanup(c: &mut PgConnection) -> Result<(), Failure> {
    execute(
        c,
        "DELETE FROM attachments WHERE message_id IS NULL AND (expires_at<=now() OR owner_id IS NULL)",
        &[],
    )?;
    Ok(())
}

/// Expired stages become unusable immediately; retry their removal every minute.
pub fn start_cleanup(pool: DbPool) {
    tokio::spawn(async move {
        let mut timer = tokio::time::interval(Duration::from_secs(60));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            timer.tick().await;
            let pool = pool.clone();
            let _ = tokio::task::spawn_blocking(move || cleanup(&mut *connection(&pool)?)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn stalled_download_deadline_releases_buffer_and_admission_without_polling() {
        use futures_util::StreamExt;
        let budget = Arc::new(Semaphore::new(1));
        let permit = Arc::new(budget.clone().try_acquire_owned().unwrap());
        let mut body = DownloadStream::new(vec![0; 128 * 1024], permit, Duration::from_millis(25));
        assert_eq!(body.next().await.unwrap().unwrap().len(), 64 * 1024);
        assert_eq!(budget.available_permits(), 0);
        drop(
            tokio::time::timeout(Duration::from_secs(2), budget.acquire())
                .await
                .unwrap()
                .unwrap(),
        );
        assert!(body.transfer.lock().unwrap().is_none());
        assert_eq!(
            body.next().await.unwrap().unwrap_err().kind(),
            std::io::ErrorKind::TimedOut
        );
        assert!(body.next().await.is_none());
    }
    #[test]
    fn active_content_and_untrusted_hints_never_classify_as_documents() {
        for bytes in [
            b"<svg onload='alert(1)'>".as_slice(),
            b"<!doctype html>",
            b"%PDF-1.7",
        ] {
            assert_eq!(classify(bytes), "application/octet-stream");
        }
        assert_eq!(classify(b"\x89PNG\r\n\x1a\nrest"), "image/png");
        assert_eq!(classify(b"\0\0\0\x18ftypisom"), "video/mp4");
        assert_eq!(
            disposition("a\"\r\n😀"),
            "attachment; filename=\"download\"; filename*=UTF-8''%61%22%0D%0A%F0%9F%98%80"
        );
    }
    #[test]
    fn filenames_and_attachment_ids_are_bounded() {
        let mut upload = AttachmentUpload {
            guild_id: GuildId::from_uuid(Uuid::nil()),
            channel_id: ChannelId::from_uuid(Uuid::nil()),
            filename: "image.png".into(),
            content_type: "image/png".into(),
        };
        assert!(validate_upload(&upload).is_ok());
        for filename in ["../file", "a\\b", "\r\n", "..", &"x".repeat(256)] {
            upload.filename = filename.into();
            assert!(validate_upload(&upload).is_err());
        }
        let id = AttachmentId::from_uuid(Uuid::nil());
        assert!(validate_ids(&[id, id]).is_err());
        assert!(
            validate_ids(
                &(0..5)
                    .map(|_| AttachmentId::from_uuid(Uuid::new_v4()))
                    .collect::<Vec<_>>()
            )
            .is_err()
        );
    }
}
