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
        })
    })
}
/// Matches the socket maintenance cadence for expiry-only authorization changes.
const VOICE_VISIBILITY_RECHECK: Duration = Duration::from_secs(10);
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
    let Ok(account_access) = socket_access(&pool, &token).await else {
        let _ = send(
            &mut socket,
            error(id, ErrorCode::Unauthorized, "Sign in again"),
        )
        .await;
        return;
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
    let Ok(Ok(account)) = authenticated else {
        let _ = send(
            &mut socket,
            error(id, ErrorCode::Unauthorized, "Sign in again"),
        )
        .await;
        return;
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
    let mut unread_dirty = false;
    let mut unread_due = None::<Instant>;
    let mut typing_expiry = None::<Instant>;
    let mut maintenance = tokio::time::interval(Duration::from_secs(10));
    maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut roster_tick = tokio::time::interval(Duration::from_secs(1));
    roster_tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watch_voice = false;
    let mut last_roster = None;
    let mut voice_visibility = None::<(Instant, access::Snapshot, Vec<ChannelId>)>;
    // Opt-in member list: the authorized DB rows with the epochs captured before
    // reading them. Online flags are in-memory and refresh every second.
    let mut watch_members = false;
    let mut members_dirty = false;
    let mut members_due = None::<Instant>;
    let mut member_list = None::<(access::Snapshot, members::MemberList)>;
    let mut last_member_list = None::<Vec<thiscord_shared::chat::GuildMember>>;
    let mut last_online = Vec::new();
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
                        messages_dirty = false; presence_dirty = false; unread_dirty = false; unread_due = None;
                        last_members.clear(); last_unread.clear();
                        watch_voice = voice_roster; last_roster = None; voice_visibility = None;
                        roster_tick.reset_immediately();
                        watch_members = member_roster; members_dirty = false; members_due = None;
                        member_list = None; last_member_list = None; last_online.clear();
                        if channel_id.is_some() && guild_id.is_none() {
                            let _ = send(&mut socket, error(id, ErrorCode::BadRequest, "A channel requires a guild")).await; break;
                        }
                        member_access = guild_id.map(|guild| access::member(guild, account));
                        member_revoked = member_access.as_ref().map(|scope| scope.subscribe());
                        guild_access = guild_id.map(access::guild);
                        guild_revoked = guild_access.as_ref().map(|scope| scope.subscribe());
                        let Some(authorized) = account_access.snapshot(guild_access.as_ref(), member_access.as_ref()) else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                        if let Some(guild) = guild_id {
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
                presence_dirty |= next.presence != versions.presence;
                if next.unread != versions.unread { unread_due.get_or_insert_with(|| Instant::now() + Duration::from_millis(75)); }
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
                let Ok(Ok(list)) = crate::performance::blocking(move || members::snapshot(&p, &t, sub.guild)).await else {
                    let _ = send(&mut socket, ServerEvent::Revoked {}).await; break;
                };
                let refresh = list.expiry(chrono::Utc::now()).map_or(MEMBER_REFRESH, |e| e.min(MEMBER_REFRESH));
                members_due = Some(Instant::now() + refresh);
                let online = list.online(crate::friends::online);
                let view = list.members(&online);
                let Some(_delivery) = authorized.clone().deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                last_online = online;
                member_list = Some((authorized, list));
                if last_member_list.as_ref() != Some(&view) {
                    last_member_list = Some(view.clone());
                    if update(&mut socket, sub, ServerEvent::Members { members: view }).await.is_err() { break; }
                }
            }
            _ = member_tick.tick(), if member_list.is_some() && subscription.is_some() => {
                let sub = subscription.unwrap();
                let (authorized, list) = member_list.as_ref().unwrap();
                let online = list.online(crate::friends::online);
                if online != last_online {
                    // Every access-changing commit for this viewer advances an epoch
                    // in this snapshot and closes the socket before reuse here.
                    let Some(_delivery) = authorized.clone().deliver() else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                    last_online = online.clone();
                    if let Some(view) = &mut last_member_list {
                        for member in view.iter_mut() { member.online = online.contains(&member.account_id); }
                    }
                    if update(&mut socket, sub, ServerEvent::MemberPresence { online }).await.is_err() { break; }
                }
            }
            _ = async { if let Some(at) = unread_due { tokio::time::sleep_until(at.into()).await; } else { std::future::pending::<()>().await; } } => {
                unread_due = None; unread_dirty = true;
            }
            _ = async {}, if messages_dirty || presence_dirty || unread_dirty => {
                let Some(sub) = subscription else { continue; };
                let Some(authorized) = account_access.snapshot(guild_access.as_ref(), member_access.as_ref()) else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                if revoked.has_changed().unwrap_or(true) || session_revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                let p = pool.clone(); let t = token.clone();
                let read_messages = std::mem::take(&mut messages_dirty);
                let read_presence = std::mem::take(&mut presence_dirty);
                let read_unread = std::mem::take(&mut unread_dirty);
                let result = crate::performance::blocking(move || {
                    store::refresh(&p, &t, sub.guild, sub.channel, cursor, store::Refresh { messages: read_messages, presence: read_presence, unread: read_unread })
                }).await;
                let Ok(Ok((unread, poll))) = result else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
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
                        if read_presence && poll.members != last_members {
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
                    if !matches!(crate::performance::blocking(move || store::presence(&p, &t, sub.guild, channel, connection, None)).await, Ok(Ok(_))) {
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
                        let Ok(Ok(channels)) = crate::performance::blocking(move || store::visible_voice_channels(&p, &t, sub.guild)).await else {
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
                        Ok(Ok(permissions)) => {
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
                    if !matches!(crate::performance::blocking(move || auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)).await, Ok(Ok(_))) { break; }
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
    let _ = crate::performance::blocking(move || store::cleanup(&pool, connection)).await;
}
