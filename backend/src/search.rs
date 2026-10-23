//! Search is authorized before reading history, inside the same locked transaction.
use crate::{
    auth::{
        self, Failure,
        store::{connection, execute, query},
    },
    db::DbPool,
};
use axum::{
    Extension, Json, Router,
    extract::{DefaultBodyLimit, State, rejection::JsonRejection},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::post,
};
use diesel::{Connection, PgConnection};
use std::sync::{Arc, OnceLock};
use thiscord_shared::{
    AccountId, RequestId, ids::ConversationId, pagination::PageCursor, permissions::Permission,
    search::*,
};
use tokio::sync::Semaphore;

pub fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(SEARCH_PATH, post(handle))
        .layer(DefaultBodyLimit::max(8192))
}
async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<Request>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(request) = body.map_err(|_| Failure::Invalid("Invalid search request"))?;
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = WORKERS
            .get_or_init(|| Arc::new(Semaphore::new(4)))
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::Limited)?;
        crate::performance::blocking(move || {
            let _permit = permit;
            dispatch(&pool, &token, request)
        })
        .await
        .map_err(|_| Failure::Unavailable)?
    }
    .await;
    let mut response = match result {
        Ok(body) => Json(body).into_response(),
        Err(error) => error.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
fn dispatch(
    pool: &DbPool,
    token: &str,
    request: Request,
) -> Result<thiscord_shared::search::Response, Failure> {
    let mut c = connection(pool)?;
    let session = auth::store::authenticate(&mut c, token)?;
    drop(c);
    auth::store::rate_limit(pool, &format!("search:{}", session.account_id), None, false)?;
    let scope = match &request {
        Request::Search { filters, .. } => {
            filters.validate().map_err(Failure::Invalid)?;
            filters.scope.clone()
        }
        Request::Context { scope, .. } => scope.clone(),
    };
    match scope {
        Scope::Guild {
            guild_id,
            channel_id,
        } => crate::chat::store::checked(pool, token, guild_id, None, None, |c, actor, state| {
            let channels: Vec<_> = state
                .channels
                .iter()
                .filter(|ch| {
                    channel_id.is_none_or(|id| id == ch.id)
                        && crate::chat::store::check_channel(
                            state,
                            actor,
                            Some(ch.id),
                            Some(Permission::ReadHistory),
                        )
                        .is_ok()
                })
                .map(|ch| ch.id)
                .collect();
            if channel_id.is_some() && channels.is_empty() {
                return Err(Failure::Forbidden);
            }
            run(
                c,
                request,
                &serde_json::to_string(
                    &serde_json::json!({"guild_id":guild_id,"channels":channels}),
                )
                .map_err(|_| Failure::Unavailable)?,
                false,
            )
        }),
        Scope::Private { conversation_id } => {
            let mut c = connection(pool)?;
            c.transaction(|c| {
                auth::store::read_session(c, token, &session)?;
                let ids = private_access(c, session.account_id, conversation_id)?;
                run(
                    c,
                    request,
                    &serde_json::to_string(&ids).map_err(|_| Failure::Unavailable)?,
                    true,
                )
            })
        }
    }
}
/// Integration seam: replace with friends::store::pair_access after the DM safety branch
/// lands. That helper must be called in this transaction for every selected pair.
/// Baseline feaae9a has no blocks/deletion columns; accepted consent is locked here.
fn private_access(
    c: &mut PgConnection,
    actor: AccountId,
    selected: Option<ConversationId>,
) -> Result<Vec<ConversationId>, Failure> {
    let ids: Vec<ConversationId> = query(
        c,
        "SELECT to_jsonb(id) AS data FROM friendships WHERE state='accepted' AND $1::uuid IN (low_id,high_id) AND ($2='' OR id=NULLIF($2,'')::uuid) ORDER BY id LIMIT 501 FOR SHARE",
        &[
            &actor.to_string(),
            &selected.map(|id| id.to_string()).unwrap_or_default(),
        ],
    )?;
    if ids.len() > 500 {
        return Err(Failure::Invalid(
            "Select a conversation to narrow this search",
        ));
    }
    if selected.is_some() && ids.is_empty() {
        return Err(Failure::Forbidden);
    }
    Ok(ids)
}
fn run(
    c: &mut PgConnection,
    request: Request,
    ids: &str,
    private: bool,
) -> Result<thiscord_shared::search::Response, Failure> {
    // No SQL identifiers or expressions come from the request.
    let (table, scope, deleted, scope_json) = if private {
        (
            "direct_messages",
            "conversation_id",
            "COALESCE((to_jsonb(m)->>'deleted')::boolean,false)",
            "jsonb_build_object('kind','private','conversation_id',m.conversation_id)",
        )
    } else {
        (
            "messages",
            "channel_id",
            "m.deleted",
            "jsonb_build_object('kind','guild','guild_id',m.guild_id,'channel_id',m.channel_id)",
        )
    };
    let projection = format!(
        "jsonb_build_object('id',m.id,'scope',{scope_json},'author_id',m.author_id,'username',COALESCE(a.username,'Deleted account'),'content',m.content,'created_at',m.created_at) AS data"
    );
    let allowed = if private {
        format!(
            "m.{scope} IN (SELECT value::uuid FROM jsonb_array_elements_text($1::jsonb)) AND NOT {deleted}"
        )
    } else {
        "m.guild_id=($1::jsonb->>'guild_id')::uuid AND m.channel_id IN (SELECT value::uuid FROM jsonb_array_elements_text($1::jsonb->'channels')) AND NOT m.deleted".to_string()
    };
    // Session pool has query deadlines; keep full text work below that outer bound.
    execute(c, "SET LOCAL statement_timeout='2000ms'", &[])?;
    match request {
        Request::Search {
            filters,
            cursor,
            limit,
        } => {
            let (at, id) = cursor
                .map(|c| c.position())
                .map(|(t, id)| (t.to_rfc3339(), id.to_string()))
                .unwrap_or_else(|| ("infinity".into(), uuid::Uuid::nil().to_string()));
            let mut messages: Vec<Hit> = query(
                c,
                &format!(
                    "SELECT {projection} FROM {table} m LEFT JOIN accounts a ON a.id=m.author_id WHERE {allowed} AND to_tsvector('simple',m.content) @@ plainto_tsquery('simple',$2) AND ($3='' OR a.username=$3) AND m.created_at>=$4::timestamptz AND m.created_at<$5::timestamptz AND (m.created_at,m.id)<($6::timestamptz,$7::uuid) ORDER BY m.created_at DESC,m.id DESC LIMIT $8::integer"
                ),
                &[
                    ids,
                    &filters.query,
                    &filters
                        .person
                        .unwrap_or_default()
                        .trim_start_matches('@')
                        .to_ascii_lowercase(),
                    &filters
                        .after
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_else(|| "-infinity".into()),
                    &filters
                        .before
                        .map(|t| t.to_rfc3339())
                        .unwrap_or_else(|| "infinity".into()),
                    &at,
                    &id,
                    &(limit.get() + 1).to_string(),
                ],
            )?;
            let more = messages.len() > usize::from(limit.get());
            messages.truncate(usize::from(limit.get()));
            let next = if more {
                messages
                    .last()
                    .map(|m| PageCursor::new(m.created_at, m.id.as_uuid()))
            } else {
                None
            };
            Ok(thiscord_shared::search::Response { messages, next })
        }
        Request::Context { message_id, .. } => {
            let target:Hit=query(c,&format!("SELECT {projection} FROM {table} m LEFT JOIN accounts a ON a.id=m.author_id WHERE {allowed} AND m.id=$2::uuid"),&[ids,&message_id.to_string()])?.pop().ok_or(Failure::Forbidden)?;
            let location = match target.scope {
                Scope::Guild {
                    channel_id: Some(id),
                    ..
                } => id.to_string(),
                Scope::Private {
                    conversation_id: Some(id),
                } => id.to_string(),
                _ => return Err(Failure::Forbidden),
            };
            let neighborhood = format!("m.{scope}=$2::uuid AND {allowed}");
            let sql = format!(
                "WITH nearby AS ((SELECT m.* FROM {table} m WHERE {neighborhood} AND (m.created_at,m.id)<($3::timestamptz,$4::uuid) ORDER BY m.created_at DESC,m.id DESC LIMIT 20) UNION ALL (SELECT m.* FROM {table} m WHERE {neighborhood} AND (m.created_at,m.id)>=($3::timestamptz,$4::uuid) ORDER BY m.created_at,m.id LIMIT 21)) SELECT {projection} FROM nearby m LEFT JOIN accounts a ON a.id=m.author_id ORDER BY m.created_at,m.id"
            );
            let messages = query(
                c,
                &sql,
                &[
                    ids,
                    &location,
                    &target.created_at.to_rfc3339(),
                    &message_id.to_string(),
                ],
            )?;
            Ok(thiscord_shared::search::Response {
                messages,
                next: None,
            })
        }
    }
}
