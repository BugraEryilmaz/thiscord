//! Owner-controlled ephemeral peers. Signaling is in-process; media uses WebRTC.
mod peer;
mod video;
use super::*;
use crate::{
    auth::{self, store as auth_store},
    chat::access as delivery,
};
use axum::response::Response;
use axum::{
    Json,
    extract::{DefaultBodyLimit, rejection::JsonRejection},
    routing::post,
};
use diesel::Connection;
use std::sync::Mutex as StdMutex;
use thiscord_shared::{AccountId, permissions::InstanceRole, voice_bots::*};

#[derive(Clone)]
pub(super) struct Context {
    pub id: AccountId,
    pub name: String,
    pub guild: GuildId,
    pub channel: ChannelId,
    pub authority: delivery::Snapshot,
}
struct Entry {
    context: Context,
    bot: Arc<StdMutex<Bot>>,
    settings: watch::Sender<Settings>,
    stop: watch::Sender<bool>,
    done: watch::Receiver<bool>,
}
fn registry() -> &'static StdMutex<HashMap<AccountId, Entry>> {
    static REGISTRY: OnceLock<StdMutex<HashMap<AccountId, Entry>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}
struct Registration(AccountId);
impl Drop for Registration {
    fn drop(&mut self) {
        registry().lock().unwrap().remove(&self.0);
    }
}

pub(super) fn authorize(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: ChannelId,
    bot: Option<&Context>,
) -> Result<(Participant, i64), Failure> {
    if let Some(bot) = bot {
        if bot.guild != guild || bot.channel != channel || bot.authority.clone().deliver().is_none()
        {
            return Err(Failure::Forbidden);
        }
        owner(pool, token)?;
    }
    let (mut participant, revision) = store::authorize(pool, token, guild, channel)?;
    if let Some(bot) = bot {
        if !participant.can_speak {
            return Err(Failure::Forbidden);
        }
        participant.account_id = bot.id;
        participant.username = format!("test-bot-{}", bot.id);
        participant.display_name = bot.name.clone();
        participant.avatar_id = None;
        participant.test_bot = true;
    }
    Ok((participant, revision))
}

fn owner(pool: &DbPool, token: &str) -> Result<delivery::Snapshot, Failure> {
    let mut c = auth_store::connection(pool)?;
    let identity = auth_store::token_session(&mut c, token)?;
    let snapshot = delivery::snapshot(
        &delivery::account(identity.account_id),
        Some(&delivery::instance_role(identity.account_id)),
    )
    .and_then(|s| s.including(&delivery::session(identity.id)))
    .ok_or(Failure::Forbidden)?;
    let session = auth_store::authenticate(&mut c, token)?;
    c.transaction::<_, Failure, _>(|c| {
        auth_store::read_session(c, token, &session)?;
        if crate::permissions::store::instance(c, session.account_id)?.role != InstanceRole::Owner {
            return Err(Failure::Forbidden);
        }
        Ok(())
    })?;
    Ok(snapshot)
}

pub(super) fn router() -> Router<Option<DbPool>> {
    Router::new()
        .route(PATH, post(handle))
        .layer(DefaultBodyLimit::max(4096))
}
async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<Request>, JsonRejection>,
) -> Response {
    let result = async {
        static WORKERS: OnceLock<Arc<Semaphore>> = OnceLock::new();
        let permit = Arc::new(
            WORKERS
                .get_or_init(|| Arc::new(Semaphore::new(4)))
                .clone()
                .try_acquire_owned()
                .map_err(|_| Failure::Limited)?,
        );
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = auth::bearer(&headers);
        let Json(request) = body.map_err(|_| Failure::Invalid("Invalid voice bot request"))?;
        let authority = {
            let pool = pool.clone();
            let token = token.clone();
            let permit = permit.clone();
            let (guild, channel) = (request.guild_id, request.channel_id);
            tokio::task::spawn_blocking(move || {
                let _permit = permit;
                let authority = owner(&pool, &token)?;
                let (participant, _) = store::authorize(&pool, &token, guild, channel)?;
                if guild.as_uuid() == channel.as_uuid() || !participant.can_speak {
                    return Err(Failure::Forbidden);
                }
                Ok::<_, Failure>(authority)
            })
            .await
            .map_err(|_| Failure::Unavailable)??
        };
        match request.command {
            Command::List {} => {}
            Command::Add { settings } => {
                settings.validate().map_err(Failure::Invalid)?;
                add(
                    pool,
                    token,
                    request.guild_id,
                    request.channel_id,
                    settings,
                    authority.clone(),
                )
                .await?;
            }
            Command::Update {
                bot_id,
                revision,
                settings,
            } => {
                settings.validate().map_err(Failure::Invalid)?;
                let entries = registry().lock().unwrap();
                let entry = scoped(&entries, bot_id, request.guild_id, request.channel_id)?;
                let _delivery = authority.clone().deliver().ok_or(Failure::Forbidden)?;
                let mut bot = entry.bot.lock().unwrap();
                if bot.revision != revision {
                    return Err(Failure::Conflict);
                }
                bot.revision += 1;
                bot.settings = settings.clone();
                entry.settings.send_replace(settings);
            }
            Command::Remove { bot_id } => {
                let mut done = {
                    let entries = registry().lock().unwrap();
                    let entry = scoped(&entries, bot_id, request.guild_id, request.channel_id)?;
                    let _delivery = authority.clone().deliver().ok_or(Failure::Forbidden)?;
                    entry.stop.send_replace(true);
                    entry.done.clone()
                };
                tokio::time::timeout(Duration::from_secs(25), done.wait_for(|done| *done))
                    .await
                    .map_err(|_| Failure::Unavailable)?
                    .map_err(|_| Failure::Unavailable)?;
            }
        }
        let _delivery = authority.deliver().ok_or(Failure::Forbidden)?;
        let mut bots: Vec<_> = registry()
            .lock()
            .unwrap()
            .values()
            .filter(|e| {
                e.context.guild == request.guild_id && e.context.channel == request.channel_id
            })
            .map(|e| e.bot.lock().unwrap().clone())
            .collect();
        bots.sort_by_key(|b| b.id);
        Ok::<_, Failure>(Json(thiscord_shared::voice_bots::Response { bots }).into_response())
    }
    .await;
    let mut response = result.unwrap_or_else(|e| e.response(id));
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}
fn scoped(
    entries: &HashMap<AccountId, Entry>,
    id: AccountId,
    guild: GuildId,
    channel: ChannelId,
) -> Result<&Entry, Failure> {
    entries
        .get(&id)
        .filter(|e| e.context.guild == guild && e.context.channel == channel)
        .ok_or(Failure::NotFound)
}

async fn add(
    pool: DbPool,
    token: String,
    guild: GuildId,
    channel: ChannelId,
    settings: Settings,
    authority: delivery::Snapshot,
) -> Result<(), Failure> {
    let id = AccountId::from_uuid(uuid::Uuid::new_v4());
    let context = Context {
        id,
        name: format!(
            "Test bot · {} · {}",
            settings.microphone_loop.label(),
            &id.to_string()[..4]
        ),
        guild,
        channel,
        authority: authority.clone(),
    };
    let bot = Arc::new(StdMutex::new(Bot {
        id,
        name: context.name.clone(),
        revision: 0,
        settings: settings.clone(),
        sent_packets: 0,
        dropped_packets: 0,
        received_packets: 0,
        output_peak: 0.0,
        video: Vec::new(),
        received_system_audio_packets: 0,
    }));
    let (settings_tx, settings_rx) = watch::channel(settings);
    let (stop_tx, stop_rx) = watch::channel(false);
    let (done_tx, done_rx) = watch::channel(false);
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    {
        let mut entries = registry().lock().unwrap();
        if entries.len() >= MAX_BOTS {
            return Err(Failure::Limited);
        }
        let _delivery = authority.deliver().ok_or(Failure::Forbidden)?;
        entries.insert(
            id,
            Entry {
                context: context.clone(),
                bot: bot.clone(),
                settings: settings_tx,
                stop: stop_tx.clone(),
                done: done_rx,
            },
        );
    }
    // Detached supervisor owns cleanup even when the creating HTTP request disappears.
    tokio::spawn(async move {
        let registration = Registration(id);
        let connection_id = RequestId::from_uuid(uuid::Uuid::new_v4());
        let (socket, tx, rx) = logging::Socket::internal(connection_id, context);
        let server = serve(socket, pool, connection_id);
        let client = peer::run(
            tx,
            rx,
            token,
            guild,
            channel,
            settings_rx,
            stop_rx,
            bot,
            ready_tx,
        );
        tokio::join!(server, client);
        drop(registration);
        done_tx.send_replace(true);
    });
    match tokio::time::timeout(Duration::from_secs(20), ready_rx).await {
        Ok(Ok(Ok(()))) => Ok(()),
        Ok(Ok(Err(failure))) => {
            stop_tx.send_replace(true);
            Err(failure)
        }
        _ => {
            stop_tx.send_replace(true);
            Err(Failure::Unavailable)
        }
    }
}
