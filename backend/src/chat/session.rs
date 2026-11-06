use super::*;
use thiscord_shared::{ChannelId, GuildId, permissions::Permissions};

#[derive(Clone, Copy)]
struct Subscription {
    id: u64,
    guild: GuildId,
    channel: Option<ChannelId>,
}
struct Snapshot {
    history: Option<History>,
    permissions: Permissions,
    unread: Vec<Unread>,
    grant: store::Grant,
}
fn snapshot(pool: &DbPool, token: &str, sub: Subscription) -> Result<Snapshot, Failure> {
    store::checked(pool, token, sub.guild, None, None, |c, actor, state| {
        let permissions =
            store::check_channel(state, actor, sub.channel, Some(Permission::ReadHistory))?;
        Ok(Snapshot {
            history: sub
                .channel
                .map(|channel| {
                    store::history(c, sub.guild, channel, None, Default::default(), actor)
                })
                .transpose()?,
            permissions,
            unread: store::unread(c, sub.guild, actor, state)?,
            grant: store::grant(state, actor),
        })
    })
}
/// Matches the socket maintenance cadence for expiry-only authorization changes.
const VOICE_VISIBILITY_RECHECK: Duration = Duration::from_secs(10);
/// Message, presence and unread delivery reuse the last full database check
/// for at most this long, and only while the access epochs captured before
/// that check are current. Access-changing commits close those epochs (and
/// this socket); time-based changes such as session expiry wait at most this.
const CHAT_AUTHORIZATION_RECHECK: Duration = Duration::from_secs(10);
/// Unread badges coalesce guild-wide message and read commits: the first change
/// after a quiet period recounts after a short debounce, later ones at most once
/// per interval. Messages in the open channel are delivered without this delay.
const UNREAD_DEBOUNCE: Duration = Duration::from_millis(75);
const UNREAD_INTERVAL: Duration = Duration::from_secs(1);
/// Presence/typing lists are sent whole, so joins in a busy channel are
/// coalesced the same way: at most one presence delivery per interval.
const PRESENCE_DEBOUNCE: Duration = Duration::from_millis(50);
const PRESENCE_INTERVAL: Duration = Duration::from_secs(1);

/// When a coalesced delivery that last ran at `last` runs for a change seen at
/// `now`: after `debounce` when quiet, otherwise no sooner than `interval` after
/// its previous run. A pending due time absorbs later changes.
fn coalesced(
    last: Option<Instant>,
    now: Instant,
    debounce: Duration,
    interval: Duration,
) -> Instant {
    last.map_or(now, |at| at + interval).max(now + debounce)
}

fn transient<T>(result: &Result<Result<T, Failure>, tokio::task::JoinError>) -> bool {
    matches!(result, Ok(Err(error)) if error.transient())
}
/// Delivery under a reusable grant. New events and the presence list come from
/// the channel's shared reads, which subscribers await without holding a
/// blocking thread; only the remaining work (a private event page, unread
/// counts) runs as one read-only snapshot on a blocking thread.
async fn refresh_reused(
    pool: &DbPool,
    sub: Subscription,
    cursor: i64,
    mut work: store::Refresh,
    granted: store::Grant,
    feed: Option<std::sync::Arc<super::feed::Feed>>,
) -> Result<Result<store::Refreshed, Failure>, tokio::task::JoinError> {
    let mut events = None;
    let mut members = None;
    if let (Some(channel), Some(feed)) = (sub.channel, feed)
        && granted.readable.contains(&channel)
    {
        if work.messages {
            events = match feed.cached(cursor) {
                Some(cached) => Some(cached),
                None => match feed.events(pool, sub.guild, channel, cursor).await {
                    Ok(shared) => shared,
                    Err(error) => return Ok(Err(error)),
                },
            };
            work.messages = events.is_none();
        }
        if work.presence {
            members = match feed.presence(pool, sub.guild, channel).await {
                Ok(list) => Some(list),
                Err(error) => return Ok(Err(error)),
            };
            work.presence = false;
        }
    }
    let (unread, poll) = if work.messages || work.presence || work.unread {
        let pool = pool.clone();
        let read = crate::performance::blocking(move || {
            store::refresh_granted(&pool, sub.guild, sub.channel, cursor, work, &granted)
        })
        .await?;
        match read {
            Ok(refreshed) => refreshed,
            Err(error) => return Ok(Err(error)),
        }
    } else {
        (None, None)
    };
    let poll = if events.is_some() || members.is_some() {
        let poll = poll.unwrap_or(store::Poll {
            events: Vec::new(),
            members: Vec::new(),
        });
        Some(store::Poll {
            events: events.unwrap_or(poll.events),
            members: members.unwrap_or(poll.members),
        })
    } else {
        poll
    };
    Ok(Ok((unread, poll)))
}

fn unavailable(id: RequestId) -> ServerEvent {
    error(
        id,
        ErrorCode::ServiceUnavailable,
        "Chat is temporarily unavailable",
    )
}
/// Member lists reload on commit notifications and status expiry; this bounds
/// staleness from writes outside those paths (unchanged lists are not resent).
const MEMBER_REFRESH: Duration = Duration::from_secs(60);

async fn update(socket: &mut WebSocket, sub: Subscription, event: ServerEvent) -> Result<(), ()> {
    send(
        socket,
        ServerEvent::Update {
            subscription: sub.id,
            event: Box::new(event),
        },
    )
    .await
}

pub(super) async fn serve(
    mut socket: WebSocket,
    pool: DbPool,
    id: RequestId,
    token: String,
    guild_updates: bool,
) {
    let account_access = match socket_access(&pool, &token).await {
        Ok(access) => access,
        Err(error) => {
            let event = if error.transient() {
                unavailable(id)
            } else {
                super::error(id, ErrorCode::Unauthorized, "Sign in again")
            };
            let _ = send(&mut socket, event).await;
            return;
        }
    };
    let mut revoked = account_access.account.subscribe();
    let mut session_revoked = account_access.session.subscribe();
    let mut member_access = None;
    let mut member_revoked = None;
    let mut guild_access = None;
    let mut guild_revoked = None;
    let Some(initial) = account_access.snapshot(None, None) else {
        return;
    };
    let mut listener = None::<notifications::Listener<'static>>;
    let mut versions = notifications::Versions::default();
    let connection = Uuid::new_v4();
    let p = pool.clone();
    let t = token.clone();
    let authenticated = crate::performance::blocking(move || {
        let session = auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)?;
        auth::store::rate_limit(&p, &format!("socket:{}", session.account_id), None, true)?;
        Ok::<_, Failure>(session.account_id)
    })
    .await;
    let account = match authenticated {
        Ok(Ok(account)) => account,
        // Database trouble is not a credential failure: let the client retry
        // instead of checking (and possibly clearing) its session.
        Ok(Err(error)) if error.transient() => {
            let _ = send(&mut socket, unavailable(id)).await;
            return;
        }
        _ => {
            let _ = send(
                &mut socket,
                error(id, ErrorCode::Unauthorized, "Sign in again"),
            )
            .await;
            return;
        }
    };
    let Some(delivery) = initial.deliver() else {
        return;
    };
    if send(&mut socket, ServerEvent::Authenticated {})
        .await
        .is_err()
    {
        return;
    }
    drop(delivery);
    let mut subscription: Option<Subscription> = None;
    let mut cursor = 0;
    let mut last_members = Vec::new();
    let mut last_unread = Vec::new();
    let mut last_permissions = Permissions::new();
    let mut last_seen = Instant::now();
    let mut window = Instant::now();
    let mut count = 0;
    let mut messages_dirty = false;
    let mut presence_dirty = false;
    let mut presence_due = None::<Instant>;
    let mut presence_at = None::<Instant>;
    let mut unread_dirty = false;
    let mut unread_due = None::<Instant>;
    // When this socket last recounted unread, for UNREAD_INTERVAL coalescing.
    let mut unread_at = None::<Instant>;
    let mut typing_expiry = None::<Instant>;
    let mut maintenance = tokio::time::interval(Duration::from_secs(10));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut roster_tick = tokio::time::interval(Duration::from_secs(1));
    roster_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watch_voice = false;
    let mut last_roster = None;
    let mut voice_visibility = None::<(Instant, access::Snapshot, Vec<ChannelId>)>;
    // The last full chat authorization: when it ran, the epochs captured before
    // it and what it established. Cleared on every subscription change.
    let mut grant = None::<(Instant, access::Snapshot, store::Grant)>;
    // The open channel's shared event reads, held while subscribed to it.
    let mut feed = None::<std::sync::Arc<super::feed::Feed>>;
    // Opt-in member list: the authorized DB rows with the epochs captured before
    // reading them. Online flags are in-memory and refresh every second.
    let mut watch_members = false;
    let mut members_dirty = false;
    let mut members_due = None::<Instant>;
    let mut member_list = None::<(access::Snapshot, members::MemberList)>;
    let mut last_member_list = None::<Vec<thiscord_shared::chat::GuildMember>>;
    let mut last_live = members::Live::default();
    // Transient database trouble delivers nothing that needs the failed read and
    // retries with backoff; the socket closes with a service error (never
    // `revoked`) only after `retry::GRACE` without success.
    let mut trouble = crate::retry::Backoff::default();
    // Delivery retry after transient database trouble; dirty flags stay set.
    let mut retry_at = None::<Instant>;
    let mut member_tick = tokio::time::interval(Duration::from_secs(1));
    member_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut guild_hints = guild_updates.then(|| super::guild_updates().subscribe());
    loop {
        tokio::select! { biased;
            _ = session_revoked.changed() => { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
            _ = access::changed(&mut member_revoked) => { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
            _ = access::changed(&mut guild_revoked) => { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
            _ = revoked.changed() => { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
            incoming = socket.recv() => {
                let Some(Ok(incoming)) = incoming else { break; };
                if matches!(incoming, Message::Close(_)) { break; }
                if window.elapsed() >= Duration::from_secs(10) { window = Instant::now(); count = 0; }
                count += 1;
                if count > 30 { let _ = send(&mut socket, error(id, ErrorCode::RateLimited, "Socket event limit exceeded")).await; break; }
                last_seen = Instant::now();
                match frame(incoming) {
                    Some(ClientEvent::Subscribe { subscription: serial, guild_id, channel_id, voice_roster, members: member_roster }) => {
                        if revoked.has_changed().unwrap_or(true) || session_revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                        let p = pool.clone();
                        let _ = crate::performance::blocking(move || store::cleanup(&p, connection)).await;
                        listener = None;
                        versions = notifications::Versions::default();
                        subscription = None; typing_expiry = None;
                        messages_dirty = false; presence_dirty = false; unread_dirty = false; unread_due = None; presence_due = None;
                        last_members.clear(); last_unread.clear();
                        watch_voice = voice_roster; last_roster = None; voice_visibility = None;
                        roster_tick.reset_immediately();
                        watch_members = member_roster; members_dirty = false; members_due = None;
                        member_list = None; last_member_list = None; last_live = members::Live::default();
                        grant = None;
                        feed = guild_id.zip(channel_id).map(|(guild, channel)| super::feed::subscribe(guild, channel));
                        if channel_id.is_some() && guild_id.is_none() {
                            let _ = send(&mut socket, error(id, ErrorCode::BadRequest, "A channel requires a guild")).await; break;
                        }
                        member_access = guild_id.map(|guild| access::member(guild, account));
                        member_revoked = member_access.as_ref().map(|scope| scope.subscribe());
                        guild_access = guild_id.map(access::guild);
                        guild_revoked = guild_access.as_ref().map(|scope| scope.subscribe());
                        let Some(authorized) = account_access.snapshot(guild_access.as_ref(), member_access.as_ref()) else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                        if let Some(guild) = guild_id {
                            let checked_at = Instant::now();
                            let sub = Subscription { id: serial, guild, channel: channel_id };
                            let p = pool.clone(); let t = token.clone();
                            let route = notifications().subscribe(guild, channel_id, account);
                            let result = crate::performance::blocking(move || {
                                let ready = snapshot(&p, &t, sub)?;
                                if let Some(channel) = sub.channel { store::presence(&p, &t, guild, channel, connection, None)?; }
                                Ok::<_, Failure>(ready)
                            }).await;
                            let Ok(Ok(ready)) = result else {
                                let _ = update(&mut socket, sub, error(id, ErrorCode::Forbidden, "Session or channel access is unavailable")).await;
                                continue;
                            };
                            grant = Some((checked_at, authorized.clone(), ready.grant));
                            let Some(_delivery) = authorized.deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                            cursor = ready.history.as_ref().map_or(0, |h| h.event_cursor);
                            last_unread = ready.unread.clone();
                            last_permissions = ready.permissions.clone();
                            if send(&mut socket, ServerEvent::Subscribed { subscription: serial, history: ready.history, permissions: ready.permissions }).await.is_err() { break; }
                            if update(&mut socket, sub, ServerEvent::Unread { channels: ready.unread }).await.is_err() { break; }
                            subscription = Some(sub); listener = Some(route); presence_dirty = sub.channel.is_some();
                            members_dirty = watch_members;
                        } else {
                            let Some(_delivery) = authorized.deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                            if send(&mut socket, ServerEvent::Subscribed { subscription: serial, history: None, permissions: Permissions::new() }).await.is_err() { break; }
                        }
                    }
                    Some(ClientEvent::Ping {}) => { if send(&mut socket, ServerEvent::Pong {}).await.is_err() { break; } }
                    Some(ClientEvent::Typing { active }) if subscription.is_some_and(|s| s.channel.is_some()) => {
                        let sub = subscription.unwrap();
                        if revoked.has_changed().unwrap_or(true) || session_revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                        let p = pool.clone(); let t = token.clone();
                        if !matches!(crate::performance::blocking(move || store::presence(&p, &t, sub.guild, sub.channel.unwrap(), connection, Some(active))).await, Ok(Ok(_))) {
                            let _ = send(&mut socket, ServerEvent::Revoked {}).await; break;
                        }
                        typing_expiry = active.then(|| Instant::now() + Duration::from_secs(4));
                    }
                    _ => { let _ = send(&mut socket, error(id, ErrorCode::BadRequest, "Invalid socket event")).await; break; }
                }
            }
            _ = async { if let Some(route) = &mut listener { let _ = route.receiver.changed().await; } else { std::future::pending::<()>().await; } } => {
                let next = *listener.as_mut().unwrap().receiver.borrow_and_update();
                messages_dirty |= next.messages != versions.messages;
                if next.presence != versions.presence {
                    presence_due.get_or_insert_with(|| coalesced(presence_at, Instant::now(), PRESENCE_DEBOUNCE, PRESENCE_INTERVAL));
                }
                if next.unread != versions.unread {
                    // A short debounce after quiet periods; busy guilds recount at most once per interval.
                    unread_due.get_or_insert_with(|| coalesced(unread_at, Instant::now(), UNREAD_DEBOUNCE, UNREAD_INTERVAL));
                }
                members_dirty |= watch_members && next.members != versions.members;
                versions = next;
            }
            _ = async { if let Some(at) = members_due { tokio::time::sleep_until(at.into()).await; } else { std::future::pending::<()>().await; } } => {
                members_due = None; members_dirty = watch_members;
            }
            _ = async {}, if members_dirty && subscription.is_some() => {
                members_dirty = false;
                let sub = subscription.unwrap();
                let Some(authorized) = account_access.snapshot(guild_access.as_ref(), member_access.as_ref()) else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                if revoked.has_changed().unwrap_or(true) || session_revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                let p = pool.clone(); let t = token.clone();
                let result = crate::performance::blocking(move || members::snapshot(&p, &t, sub.guild)).await;
                if transient(&result) {
                    let Some(delay) = trouble.failed() else { let _ = send(&mut socket, unavailable(id)).await; break; };
                    members_due = Some(Instant::now() + delay);
                    continue;
                }
                let Ok(Ok(list)) = result else {
                    let _ = send(&mut socket, ServerEvent::Revoked {}).await; break;
                };
                let refresh = list.expiry(chrono::Utc::now()).map_or(MEMBER_REFRESH, |e| e.min(MEMBER_REFRESH));
                members_due = Some(Instant::now() + refresh);
                let live = list.live(crate::friends::online, crate::friends::activity);
                let view = list.members(&live);
                let Some(_delivery) = authorized.clone().deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                last_live = live;
                member_list = Some((authorized, list));
                if last_member_list.as_ref() != Some(&view) {
                    last_member_list = Some(view.clone());
                    if update(&mut socket, sub, ServerEvent::Members { members: view }).await.is_err() { break; }
                }
            }
            _ = member_tick.tick(), if member_list.is_some() && subscription.is_some() => {
                let sub = subscription.unwrap();
                let (authorized, list) = member_list.as_ref().unwrap();
                let live = list.live(crate::friends::online, crate::friends::activity);
                if live != last_live {
                    // Every access-changing commit for this viewer advances an epoch
                    // in this snapshot and closes the socket before reuse here.
                    let Some(_delivery) = authorized.clone().deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                    if let Some(view) = &mut last_member_list { live.apply(view); }
                    let event = ServerEvent::MemberPresence { online: live.online.clone(), activities: live.activities.clone() };
                    last_live = live;
                    if update(&mut socket, sub, event).await.is_err() { break; }
                }
            }
            _ = async { if let Some(at) = presence_due { tokio::time::sleep_until(at.into()).await; } else { std::future::pending::<()>().await; } } => {
                presence_due = None; presence_dirty = true; presence_at = Some(Instant::now());
            }
            _ = async { if let Some(at) = unread_due { tokio::time::sleep_until(at.into()).await; } else { std::future::pending::<()>().await; } } => {
                unread_due = None; unread_dirty = true; unread_at = Some(Instant::now());
            }
            _ = async { if let Some(at) = retry_at { tokio::time::sleep_until(at.into()).await; } else { std::future::pending::<()>().await; } } => {
                retry_at = None;
            }
            _ = async {}, if (messages_dirty || presence_dirty || unread_dirty) && retry_at.is_none() => {
                let Some(sub) = subscription else { continue; };
                if revoked.has_changed().unwrap_or(true) || session_revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                let p = pool.clone();
                // Fan-out reuses the last full check while its epochs are current;
                // otherwise authenticate and re-evaluate grants under the guild lock.
                let reusable = grant.as_ref()
                    .filter(|(at, authorized, _)| at.elapsed() < CHAT_AUTHORIZATION_RECHECK && authorized.current())
                    .map(|(_, authorized, granted)| (authorized.clone(), granted.clone()));
                // With a reusable grant, messages and presence come from shared reads;
                // never make them wait behind this subscriber's unread query, which
                // runs on the next pass instead.
                let defer_unread = reusable.is_some() && (messages_dirty || presence_dirty);
                let work = store::Refresh {
                    messages: std::mem::take(&mut messages_dirty),
                    presence: std::mem::take(&mut presence_dirty),
                    unread: !defer_unread && std::mem::take(&mut unread_dirty),
                };
                let (authorized, result) = if let Some((authorized, granted)) = reusable {
                    (authorized, refresh_reused(&pool, sub, cursor, work, granted, feed.clone()).await)
                } else {
                    let Some(authorized) = account_access.snapshot(guild_access.as_ref(), member_access.as_ref()) else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                    let checked_at = Instant::now();
                    let t = token.clone();
                    let result = crate::performance::blocking(move || {
                        store::refresh(&p, &t, sub.guild, sub.channel, cursor, work)
                    }).await;
                    let result = result.map(|r| r.map(|(refreshed, granted)| {
                        grant = Some((checked_at, authorized.clone(), granted));
                        refreshed
                    }));
                    (authorized, result)
                };
                if transient(&result) {
                    // Fail closed: deliver nothing, keep the work and retry shortly.
                    let Some(delay) = trouble.failed() else { let _ = send(&mut socket, unavailable(id)).await; break; };
                    messages_dirty |= work.messages; presence_dirty |= work.presence; unread_dirty |= work.unread;
                    retry_at = Some(Instant::now() + delay);
                    continue;
                }
                let Ok(Ok((unread, poll))) = result else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                trouble.clear();
                let Some(_delivery) = authorized.deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                let delivery = async {
                    if let Some(unread) = unread && unread != last_unread {
                        last_unread = unread.clone();
                        update(&mut socket, sub, ServerEvent::Unread { channels: unread }).await?;
                    }
                    if let Some(poll) = poll {
                        messages_dirty = poll.events.len() == 100;
                        for (sequence, message) in poll.events {
                            update(&mut socket, sub, ServerEvent::Message { message: Box::new(message), cursor: sequence }).await?;
                            cursor = sequence;
                        }
                        if work.presence && poll.members != last_members {
                            last_members = poll.members.clone();
                            update(&mut socket, sub, ServerEvent::Presence { members: poll.members }).await?;
                        }
                    }
                    Ok::<(), ()>(())
                };
                if !matches!(tokio::time::timeout(Duration::from_secs(2), delivery).await, Ok(Ok(()))) { break; }
            }
            _ = async { if let Some(at) = typing_expiry { tokio::time::sleep_until(at.into()).await; } else { std::future::pending::<()>().await; } } => {
                typing_expiry = None;
                if let Some(sub) = subscription && let Some(channel) = sub.channel {
                    if revoked.has_changed().unwrap_or(true) || session_revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                    let p = pool.clone(); let t = token.clone();
                    let result = crate::performance::blocking(move || store::presence(&p, &t, sub.guild, channel, connection, None)).await;
                    // Typing also expires server-side; a transient failure only delays clearing it.
                    if transient(&result) {
                        if trouble.failed().is_none() { let _ = send(&mut socket, unavailable(id)).await; break; }
                    } else if !matches!(result, Ok(Ok(_))) {
                        let _ = send(&mut socket, ServerEvent::Revoked {}).await; break;
                    }
                }
            }
            _ = roster_tick.tick(), if watch_voice && subscription.is_some() => {
                let sub = subscription.unwrap();
                // Every visibility-changing commit advances an epoch in this snapshot
                // and closes the socket, so reuse the authorization epoch with its
                // result. Recheck time-based state (session expiry, timeouts) regularly.
                let (authorized, channels) = match &voice_visibility {
                    Some((at, authorized, channels)) if at.elapsed() < VOICE_VISIBILITY_RECHECK => (authorized.clone(), channels.clone()),
                    _ => {
                        let checked = Instant::now();
                        let Some(authorized) = account_access.snapshot(guild_access.as_ref(), member_access.as_ref()) else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                        let p = pool.clone(); let t = token.clone();
                        let result = crate::performance::blocking(move || store::visible_voice_channels(&p, &t, sub.guild)).await;
                        if transient(&result) {
                            // No roster without a current check; the next tick retries.
                            if trouble.failed().is_none() { let _ = send(&mut socket, unavailable(id)).await; break; }
                            continue;
                        }
                        let Ok(Ok(channels)) = result else {
                            let _ = send(&mut socket, ServerEvent::Revoked {}).await; break;
                        };
                        voice_visibility = Some((checked, authorized.clone(), channels.clone()));
                        (authorized, channels)
                    }
                };
                let channels = crate::voice::roster(channels).await;
                if last_roster.as_ref() == Some(&channels) {
                    crate::performance::ROSTER_UNCHANGED.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
                let Some(_delivery) = authorized.deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                if last_roster.as_ref() != Some(&channels) {
                    last_roster = Some(channels.clone());
                    if update(&mut socket, sub, ServerEvent::VoiceRoster { channels }).await.is_err() { break; }
                }
            }
            hint = async { match &mut guild_hints { Some(hints) => hints.recv().await, None => std::future::pending().await } } => {
                use tokio::sync::broadcast::error::RecvError;
                let guild = match hint {
                    Ok(guild) => guild,
                    // Hints are advisory; a missed one is recovered by the next refresh.
                    Err(RecvError::Lagged(_)) => continue,
                    Err(RecvError::Closed) => { guild_hints = None; continue; }
                };
                let Some(authorized) = account_access.snapshot(None, None) else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                let p = pool.clone();
                let member = crate::performance::blocking(move || {
                    let mut c = auth::store::connection(&p)?;
                    Ok::<_, Failure>(auth::store::query::<bool>(&mut c, "SELECT to_jsonb(EXISTS(SELECT 1 FROM guild_members WHERE guild_id=$1::uuid AND account_id=$2::uuid)) AS data", &[&guild.to_string(), &account.to_string()])? == [true])
                }).await;
                if !matches!(member, Ok(Ok(true))) { continue; }
                let Some(_delivery) = authorized.deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                if send(&mut socket, ServerEvent::GuildUpdated { guild_id: guild }).await.is_err() { break; }
            }
            _ = maintenance.tick() => {
                if last_seen.elapsed() > Duration::from_secs(35) { break; }
                // Expiry/authentication and presence lease maintenance only;
                // committed changes independently wake delivery immediately.
                if let Some(sub) = subscription {
                    let p = pool.clone(); let t = token.clone();
                    match crate::performance::blocking(move || store::maintain(&p, &t, sub.guild, sub.channel, connection)).await {
                        Ok(Err(error)) if error.transient() => {
                            if trouble.failed().is_none() { let _ = send(&mut socket, unavailable(id)).await; break; }
                        }
                        Ok(Ok(permissions)) => {
                            trouble.clear();
                            // Expiring timeouts change grants without a write. Reconnect
                            // to refresh the client composer from an authorized snapshot.
                            if permissions != last_permissions {
                                let _ = send(&mut socket, ServerEvent::Revoked {}).await;
                                break;
                            }
                        },
                        _ => { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                    }
                } else {
                    let p = pool.clone(); let t = token.clone();
                    let result = crate::performance::blocking(move || auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)).await;
                    if transient(&result) {
                        if trouble.failed().is_none() { let _ = send(&mut socket, unavailable(id)).await; break; }
                    } else if !matches!(result, Ok(Ok(_))) { break; }
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
    let _ = crate::performance::blocking(move || store::cleanup(&pool, connection)).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coalescing_debounces_quiet_changes_and_spaces_busy_ones() {
        let now = Instant::now();
        let ms = Duration::from_millis;
        // Quiet: only the short debounce.
        assert_eq!(coalesced(None, now, ms(75), ms(1000)), now + ms(75));
        assert_eq!(
            coalesced(Some(now - ms(5000)), now, ms(75), ms(1000)),
            now + ms(75)
        );
        // Busy: no sooner than one interval after the previous run.
        assert_eq!(
            coalesced(Some(now - ms(200)), now, ms(75), ms(1000)),
            now + ms(800)
        );
        assert_eq!(
            coalesced(Some(now - ms(950)), now, ms(75), ms(1000)),
            now + ms(75)
        );
    }
}
