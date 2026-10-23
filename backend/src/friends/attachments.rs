//! Private byte storage; all reads recheck current consent and message lifetime.
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
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use diesel::{
    prelude::*,
    sql_types::{Binary, Text, Uuid as SqlUuid},
};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use thiscord_shared::{
    AccountId, AttachmentId, MessageId, RequestId,
    chat::*,
    friends::{DIRECT_ATTACHMENTS_PATH, DirectAttachmentUpload},
    ids::ConversationId,
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

pub(super) fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(DIRECT_ATTACHMENTS_PATH, post(upload))
        .route("/api/v1/direct-attachments/{id}", get(download))
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
    let mut r = result.unwrap_or_else(|e| e.response(id));
    r.headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    r.headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    r
}
fn validate(u: &DirectAttachmentUpload) -> Result<(), Failure> {
    if u.filename.trim().is_empty()
        || u.filename.len() > MAX_ATTACHMENT_FILENAME_BYTES
        || u.filename
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\'))
        || matches!(u.filename.as_str(), "." | "..")
        || u.content_type.len() > 127
        || u.content_type.chars().any(char::is_control)
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
    query: Result<Query<DirectAttachmentUpload>, QueryRejection>,
    request: Request,
) -> Response {
    let result=async {
        let permit=admission()?;
        let Query(upload)=query.map_err(|_|Failure::Invalid("Invalid private attachment query"))?;validate(&upload)?;
        let pool=pool.ok_or(Failure::Unavailable)?;let token=auth::bearer(request.headers());
        let (p,t,u,guard)=(pool.clone(),token.clone(),upload.clone(),permit.clone());
        tokio::task::spawn_blocking(move ||{let _guard=guard;store::checked(&p,&t,u.conversation_id,false,|c,actor|quota(c,actor,0))}).await.map_err(|_|Failure::Unavailable)??;
        let bytes=tokio::time::timeout(Duration::from_secs(30),to_bytes(request.into_body(),MAX_ATTACHMENT_BYTES)).await.map_err(|_|Failure::Invalid("Attachment upload timed out"))?.map_err(|_|Failure::Invalid("Attachment exceeds 25 MiB"))?;
        if bytes.is_empty(){return Err(Failure::Invalid("Attachment is empty"));}
        let result=tokio::task::spawn_blocking(move || {
            let _permit=permit;
            store::checked(&pool,&token,upload.conversation_id,true,|c,actor| {
                execute(c,"SELECT pg_advisory_xact_lock(hashtextextended($1,0))", &[&format!("direct-attachment:{actor}")])?;
                cleanup(c)?;quota(c,actor,bytes.len())?;
                let a=ChatAttachment {id:AttachmentId::from_uuid(Uuid::new_v4()),filename:upload.filename,content_type:crate::chat::attachments::classify(&bytes).into(),size_bytes:bytes.len() as u64};
                diesel::sql_query("INSERT INTO direct_attachments(id,conversation_id,owner_id,filename,content_type,size_bytes,data) VALUES($1,$2,$3,$4,$5,octet_length($6),$6)")
                    .bind::<SqlUuid,_>(a.id.as_uuid()).bind::<SqlUuid,_>(upload.conversation_id.as_uuid()).bind::<SqlUuid,_>(actor.as_uuid())
                    .bind::<Text,_>(&a.filename).bind::<Text,_>(&a.content_type).bind::<Binary,_>(bytes.as_ref()).execute(c)?;
                Ok(a)
            })
        }).await.map_err(|_|Failure::Unavailable)??;
        Ok(Json(result).into_response())
    }.await;
    response(result, id)
}
fn quota(c: &mut PgConnection, actor: AccountId, incoming: usize) -> Result<(), Failure> {
    let totals: Vec<(i64, i64)> = query(
        c,
        "SELECT jsonb_build_array(count(*),COALESCE(sum(size_bytes),0)) AS data FROM direct_attachments WHERE owner_id=$1::uuid AND message_id IS NULL AND expires_at>now()",
        &[&actor.to_string()],
    )?;
    let (count, bytes) = totals.first().copied().ok_or(Failure::Unavailable)?;
    if count >= MAX_STAGED_ATTACHMENTS as i64
        || bytes.saturating_add(incoming as i64) > MAX_STAGED_ATTACHMENT_BYTES as i64
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
    conversation: ConversationId,
    message: MessageId,
    ids: &[AttachmentId],
) -> Result<(), Failure> {
    let mut metadata = Vec::<ChatAttachment>::new();
    for id in ids {
        metadata.push(query(c,"UPDATE direct_attachments SET message_id=$1::uuid WHERE id=$2::uuid AND owner_id=$3::uuid AND conversation_id=$4::uuid AND message_id IS NULL AND expires_at>now() RETURNING jsonb_build_object('id',id,'filename',filename,'content_type',content_type,'size_bytes',size_bytes) AS data", &[&message.to_string(),&id.to_string(),&actor.to_string(),&conversation.to_string()])?.pop().ok_or(Failure::Forbidden)?);
    }
    execute(
        c,
        "UPDATE direct_messages SET attachments=$2::jsonb WHERE id=$1::uuid",
        &[
            &message.to_string(),
            &serde_json::to_string(&metadata).map_err(|_| Failure::Unavailable)?,
        ],
    )?;
    Ok(())
}
#[derive(QueryableByName)]
struct File {
    #[diesel(sql_type=Binary)]
    data: Vec<u8>,
    #[diesel(sql_type=Text)]
    filename: String,
    #[diesel(sql_type=Text)]
    content_type: String,
}
async fn download(
    State(pool): State<Option<DbPool>>,
    Extension(request_id): Extension<RequestId>,
    path: Result<Path<AttachmentId>, PathRejection>,
    headers: HeaderMap,
) -> Response {
    let result=async {
        let permit=admission()?;let Path(id)=path.map_err(|_|Failure::Invalid("Invalid attachment ID"))?;
        let pool=pool.ok_or(Failure::Unavailable)?;let token=auth::bearer(&headers);let guard=permit.clone();
        let (file,epoch)=tokio::task::spawn_blocking(move || {
            let _guard=guard;let mut c=connection(&pool)?;let session=auth::store::authenticate(&mut c,&token)?;
            let mut epoch=crate::chat::access::snapshot(&crate::chat::access::account(session.account_id),Some(&crate::chat::access::session(session.id))).ok_or(Failure::Forbidden)?;
            let file=c.transaction::<_,Failure,_>(|c| {
                auth::store::read_session(c,&token,&session)?;
                let (conversation,message):(ConversationId,MessageId)=query(c,"SELECT jsonb_build_array(conversation_id,message_id) AS data FROM direct_attachments WHERE id=$1::uuid AND message_id IS NOT NULL", &[&id.to_string()])?.pop().ok_or(Failure::Forbidden)?;
                epoch=epoch.clone().including(&crate::chat::access::direct_message(message)).ok_or(Failure::Forbidden)?;
                store::pair_access(c,conversation,session.account_id,false)?;
                diesel::sql_query("SELECT a.data,a.filename,a.content_type FROM direct_attachments a JOIN direct_messages m ON m.id=a.message_id AND m.conversation_id=a.conversation_id WHERE a.id=$1 AND NOT m.deleted")
                    .bind::<SqlUuid,_>(id.as_uuid()).get_result::<File>(c).optional()?.ok_or(Failure::Forbidden)
            })?;
            Ok::<_,Failure>((file,epoch))
        }).await.map_err(|_|Failure::Unavailable)??;
        let _admission=epoch.clone().deliver().ok_or(Failure::Forbidden)?;
        let disposition=crate::chat::attachments::disposition(&file.filename);let length=file.data.len().to_string();
        let stream=PrivateStream {inner:Some(crate::chat::attachments::DownloadStream::new(file.data,permit,Duration::from_secs(60))),epoch,done:false};
        Ok(([("content-type",file.content_type),("content-disposition",disposition),("content-length",length)],Body::from_stream(stream)).into_response())
    }.await;
    response(result, request_id)
}
struct PrivateStream {
    inner: Option<crate::chat::attachments::DownloadStream>,
    epoch: crate::chat::access::Snapshot,
    done: bool,
}
impl futures_util::Stream for PrivateStream {
    type Item = Result<Bytes, std::io::Error>;
    fn poll_next(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if self.done {
            return std::task::Poll::Ready(None);
        }
        let Some(_delivery) = self.epoch.clone().deliver() else {
            self.done = true;
            self.inner.take();
            return std::task::Poll::Ready(Some(Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Private attachment access revoked",
            ))));
        };
        std::pin::Pin::new(self.inner.as_mut().expect("active transfer")).poll_next(cx)
    }
}
fn cleanup(c: &mut PgConnection) -> Result<(), Failure> {
    // Recheck eligibility after a concurrent send releases its row lock. The
    // bounded subquery can still see the old, unclaimed version of that row.
    execute(
        c,
        "DELETE FROM direct_attachments WHERE message_id IS NULL AND expires_at<=now() AND id IN (SELECT id FROM direct_attachments WHERE message_id IS NULL AND expires_at<=now() ORDER BY expires_at LIMIT 64)",
        &[],
    )?;
    Ok(())
}
pub fn start_cleanup(pool: DbPool) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(60));
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tick.tick().await;
            let p = pool.clone();
            let _ = tokio::task::spawn_blocking(move || cleanup(&mut *connection(&p)?)).await;
        }
    });
}

#[cfg(test)]
mod tests;
