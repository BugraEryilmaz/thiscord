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
                .map(|channel| store::history(c, sub.guild, channel, None, Default::default()))
                .transpose()?,
            permissions,
            unread: store::unread(c, sub.guild, actor, state)?,
        })
    })
}
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
    mut revoked: watch::Receiver<u64>,
) {
    let mut listener = None::<notifications::Listener<'static>>;
    let mut versions = notifications::Versions::default();
    let connection = Uuid::new_v4();
    let p = pool.clone();
    let t = token.clone();
    let authenticated = tokio::task::spawn_blocking(move || {
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
    if send(&mut socket, ServerEvent::Authenticated {})
        .await
        .is_err()
    {
        return;
    }
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
    loop {
        tokio::select! { biased;
            _ = revoked.changed() => { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
            incoming = socket.recv() => {
                let Some(Ok(incoming)) = incoming else { break; };
                if matches!(incoming, Message::Close(_)) { break; }
                if window.elapsed() >= Duration::from_secs(10) { window = Instant::now(); count = 0; }
                count += 1;
                if count > 30 { let _ = send(&mut socket, error(id, ErrorCode::RateLimited, "Socket event limit exceeded")).await; break; }
                last_seen = Instant::now();
                match frame(incoming) {
                    Some(ClientEvent::Subscribe { subscription: serial, guild_id, channel_id }) => {
                        let _guard = gate().read().await;
                        if revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                        let p = pool.clone();
                        let _ = tokio::task::spawn_blocking(move || store::cleanup(&p, connection)).await;
                        listener = None;
                        versions = notifications::Versions::default();
                        subscription = None; typing_expiry = None;
                        messages_dirty = false; presence_dirty = false; unread_dirty = false; unread_due = None;
                        last_members.clear(); last_unread.clear();
                        if channel_id.is_some() && guild_id.is_none() {
                            let _ = send(&mut socket, error(id, ErrorCode::BadRequest, "A channel requires a guild")).await; break;
                        }
                        if let Some(guild) = guild_id {
                            let sub = Subscription { id: serial, guild, channel: channel_id };
                            let p = pool.clone(); let t = token.clone();
                            let route = notifications().subscribe(guild, channel_id, account);
                            let result = tokio::task::spawn_blocking(move || {
                                let ready = snapshot(&p, &t, sub)?;
                                if let Some(channel) = sub.channel { store::presence(&p, &t, guild, channel, connection, None)?; }
                                Ok::<_, Failure>(ready)
                            }).await;
                            let Ok(Ok(ready)) = result else {
                                let _ = update(&mut socket, sub, error(id, ErrorCode::Forbidden, "Session or channel access is unavailable")).await;
                                continue;
                            };
                            cursor = ready.history.as_ref().map_or(0, |h| h.event_cursor);
                            last_unread = ready.unread.clone();
                            last_permissions = ready.permissions.clone();
                            if send(&mut socket, ServerEvent::Subscribed { subscription: serial, history: ready.history, permissions: ready.permissions }).await.is_err() { break; }
                            if update(&mut socket, sub, ServerEvent::Unread { channels: ready.unread }).await.is_err() { break; }
                            subscription = Some(sub); listener = Some(route); presence_dirty = sub.channel.is_some();
                        } else if send(&mut socket, ServerEvent::Subscribed { subscription: serial, history: None, permissions: Permissions::new() }).await.is_err() { break; }
                    }
                    Some(ClientEvent::Ping {}) => { if send(&mut socket, ServerEvent::Pong {}).await.is_err() { break; } }
                    Some(ClientEvent::Typing { active }) if subscription.is_some_and(|s| s.channel.is_some()) => {
                        let sub = subscription.unwrap();
                        let _guard = gate().read().await;
                        if revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                        let p = pool.clone(); let t = token.clone();
                        if !matches!(tokio::task::spawn_blocking(move || store::presence(&p, &t, sub.guild, sub.channel.unwrap(), connection, Some(active))).await, Ok(Ok(_))) {
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
                versions = next;
            }
            _ = async { if let Some(at) = unread_due { tokio::time::sleep_until(at.into()).await; } else { std::future::pending::<()>().await; } } => {
                unread_due = None; unread_dirty = true;
            }
            _ = async {}, if messages_dirty || presence_dirty || unread_dirty => {
                let Some(sub) = subscription else { continue; };
                let _guard = gate().read().await;
                if revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                let p = pool.clone(); let t = token.clone();
                let read_messages = std::mem::take(&mut messages_dirty);
                let read_presence = std::mem::take(&mut presence_dirty);
                let read_unread = std::mem::take(&mut unread_dirty);
                let result = tokio::task::spawn_blocking(move || {
                    store::refresh(&p, &t, sub.guild, sub.channel, cursor, store::Refresh { messages: read_messages, presence: read_presence, unread: read_unread })
                }).await;
                let Ok(Ok((unread, poll))) = result else { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; };
                let delivery = async {
                    if let Some(unread) = unread && unread != last_unread {
                        last_unread = unread.clone();
                        update(&mut socket, sub, ServerEvent::Unread { channels: unread }).await?;
                    }
                    if let Some(poll) = poll {
                        messages_dirty = poll.events.len() == 100;
                        for (sequence, message) in poll.events {
                            update(&mut socket, sub, ServerEvent::Message { message, cursor: sequence }).await?;
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
                    let _guard = gate().read().await;
                    if revoked.has_changed().unwrap_or(true) { let _ = send(&mut socket, ServerEvent::Revoked {}).await; break; }
                    let p = pool.clone(); let t = token.clone();
                    if !matches!(tokio::task::spawn_blocking(move || store::presence(&p, &t, sub.guild, channel, connection, None)).await, Ok(Ok(_))) {
                        let _ = send(&mut socket, ServerEvent::Revoked {}).await; break;
                    }
                }
            }
            _ = maintenance.tick() => {
                if last_seen.elapsed() > Duration::from_secs(35) { break; }
                // Expiry/authentication and presence lease maintenance only;
                // committed changes independently wake delivery immediately.
                if let Some(sub) = subscription {
                    let _guard = gate().read().await;
                    let p = pool.clone(); let t = token.clone();
                    match tokio::task::spawn_blocking(move || store::maintain(&p, &t, sub.guild, sub.channel, connection)).await {
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
                    if !matches!(tokio::task::spawn_blocking(move || auth::store::authenticate(&mut *auth::store::connection(&p)?, &t)).await, Ok(Ok(_))) { break; }
                }
            }
        }
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), socket.send(Message::Close(None))).await;
    let _ = tokio::task::spawn_blocking(move || store::cleanup(&pool, connection)).await;
}
