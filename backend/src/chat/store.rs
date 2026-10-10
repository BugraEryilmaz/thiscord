use crate::{
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
    permissions::{authorization::load, evaluator::effective},
};
use diesel::{Connection, PgConnection};
use thiscord_shared::{
    AccountId, ChannelId, GuildId, MessageId,
    chat::*,
    pagination::{PageCursor, PageSize},
    permissions::*,
};
use uuid::Uuid;

pub(super) fn checked<T>(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: Option<ChannelId>,
    permission: Option<Permission>,
    f: impl FnOnce(&mut PgConnection, AccountId, &GuildState) -> Result<T, Failure>,
) -> Result<T, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    checked_session(&mut c, token, &session, guild, channel, permission, f)
}
fn checked_session<T>(
    c: &mut PgConnection,
    token: &str,
    session: &auth::Session,
    guild: GuildId,
    channel: Option<ChannelId>,
    permission: Option<Permission>,
    f: impl FnOnce(&mut PgConnection, AccountId, &GuildState) -> Result<T, Failure>,
) -> Result<T, Failure> {
    c.transaction(|c| {
        auth::read_session(c, token, session)?;
        execute(
            c,
            "SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE",
            &[&guild.to_string()],
        )?;
        let state = load(c, guild, &[session.account_id], channel)?;
        check_channel(&state, session.account_id, channel, permission)?;
        f(c, session.account_id, &state)
    })
}
pub(super) fn check_channel(
    state: &GuildState,
    actor: AccountId,
    channel: Option<ChannelId>,
    permission: Option<Permission>,
) -> Result<Permissions, Failure> {
    if !state.members.iter().any(|m| m.account_id == actor) {
        return Err(Failure::Forbidden);
    }
    let permissions = effective(state, actor, channel);
    if let Some(ch) = channel
        && (!state
            .channels
            .iter()
            .any(|c| c.id == ch && c.kind == ChannelKind::Text)
            || !permissions.contains(&Permission::ViewChannel)
            || permission.is_some_and(|p| !permissions.contains(&p)))
    {
        return Err(Failure::Forbidden);
    }
    Ok(permissions)
}
fn message(c: &mut PgConnection, id: MessageId) -> Result<ChatMessage, Failure> {
    query(c,"SELECT to_jsonb(m)||jsonb_build_object('username',COALESCE(a.username,'Deleted account'),'display_name',COALESCE(NULLIF(a.display_name,''),a.username,'Deleted account')) AS data FROM messages m LEFT JOIN accounts a ON a.id=m.author_id WHERE m.id=$1::uuid", &[&id.to_string()])?.pop().ok_or(Failure::Forbidden)
}
fn text(value: &str) -> Result<(), Failure> {
    if value.trim().is_empty()
        || value.chars().count() > MAX_MESSAGE_CHARS
        || value
            .chars()
            .any(|c| c.is_control() && c != '\n' && c != '\t')
    {
        return Err(Failure::Invalid(
            "Message must contain 1 to 4000 characters without control codes",
        ));
    }
    Ok(())
}
fn mentions(c: &mut PgConnection, guild: GuildId, content: &str) -> Result<String, Failure> {
    let names: std::collections::BTreeSet<String> = content
        .split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
        .map(|name| {
            name.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|name| !name.is_empty())
        .collect();
    let ids: Vec<AccountId> = if names.is_empty() {
        Vec::new()
    } else {
        query(
            c,
            "SELECT to_jsonb(a.id) AS data FROM accounts a JOIN guild_members m ON m.account_id=a.id WHERE m.guild_id=$1::uuid AND a.username IN (SELECT value FROM jsonb_array_elements_text($2::jsonb)) ORDER BY a.id",
            &[
                &guild.to_string(),
                &serde_json::to_string(&names).map_err(|_| Failure::Unavailable)?,
            ],
        )?
    };
    if ids.len() > 20 {
        return Err(Failure::Invalid("At most 20 mentions per message"));
    }
    serde_json::to_string(&ids).map_err(|_| Failure::Unavailable)
}
pub(super) fn history(
    c: &mut PgConnection,
    guild: GuildId,
    channel: ChannelId,
    before: Option<PageCursor>,
    limit: PageSize,
) -> Result<History, Failure> {
    // Keep history and its event cursor consistent with same-channel commits.
    execute(
        c,
        "SELECT id FROM channels WHERE guild_id=$1::uuid AND id=$2::uuid FOR SHARE",
        &[&guild.to_string(), &channel.to_string()],
    )?;
    let (at, id) = before
        .map(|p| {
            let (at, id) = p.position();
            (at.to_rfc3339(), id.to_string())
        })
        .unwrap_or_else(|| ("infinity".into(), Uuid::nil().to_string()));
    let mut messages: Vec<ChatMessage> = query(
        c,
        "SELECT to_jsonb(m)||jsonb_build_object('username',COALESCE(a.username,'Deleted account'),'display_name',COALESCE(NULLIF(a.display_name,''),a.username,'Deleted account')) AS data FROM messages m LEFT JOIN accounts a ON a.id=m.author_id WHERE m.guild_id=$1::uuid AND m.channel_id=$2::uuid AND (m.created_at,m.id)<($3::timestamptz,$4::uuid) ORDER BY m.created_at DESC,m.id DESC LIMIT $5::integer",
        &[
            &guild.to_string(),
            &channel.to_string(),
            &at,
            &id,
            &(limit.get() + 1).to_string(),
        ],
    )?;
    let more = messages.len() > usize::from(limit.get());
    messages.truncate(usize::from(limit.get()));
    messages.reverse();
    let older = if more {
        messages
            .first()
            .map(|m| PageCursor::new(m.created_at, m.id.as_uuid()))
    } else {
        None
    };
    let event_cursor=query::<i64>(c,"SELECT to_jsonb(COALESCE(max(sequence),0)) AS data FROM message_events WHERE guild_id=$1::uuid AND channel_id=$2::uuid", &[&guild.to_string(),&channel.to_string()])?.pop().unwrap_or(0);
    Ok(History {
        messages,
        older,
        event_cursor,
    })
}
pub(super) fn dispatch(
    pool: &DbPool,
    token: &str,
    command: ChatRequest,
) -> Result<ChatResponse, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    drop(c);
    auth::rate_limit(
        pool,
        &format!("chat:{}", session.account_id),
        None,
        matches!(
            command,
            ChatRequest::Send { .. } | ChatRequest::Edit { .. } | ChatRequest::Delete { .. }
        ),
    )?;
    let (guild, channel, need) = match &command {
        ChatRequest::Unread { guild_id } => (*guild_id, None, None),
        ChatRequest::History {
            guild_id,
            channel_id,
            ..
        }
        | ChatRequest::Read {
            guild_id,
            channel_id,
            ..
        } => (*guild_id, Some(*channel_id), Some(Permission::ReadHistory)),
        ChatRequest::Send {
            guild_id,
            channel_id,
            ..
        } => (*guild_id, Some(*channel_id), Some(Permission::SendMessages)),
        ChatRequest::Edit {
            guild_id,
            channel_id,
            ..
        }
        | ChatRequest::Delete {
            guild_id,
            channel_id,
            ..
        } => (*guild_id, Some(*channel_id), None),
    };
    let mut c = connection(pool)?;
    checked_session(
        &mut c,
        token,
        &session,
        guild,
        channel,
        need,
        |c, actor, state| {
            if matches!(
                command,
                ChatRequest::Send { .. } | ChatRequest::Edit { .. } | ChatRequest::Delete { .. }
            ) {
                execute(
                    c,
                    "SELECT id FROM channels WHERE guild_id=$1::uuid AND id=$2::uuid FOR UPDATE",
                    &[
                        &guild.to_string(),
                        &channel.ok_or(Failure::Forbidden)?.to_string(),
                    ],
                )?;
            }
            match command {
                ChatRequest::History {
                    channel_id,
                    before,
                    limit,
                    ..
                } => Ok(ChatResponse::History {
                    history: history(c, guild, channel_id, before, limit)?,
                }),
                ChatRequest::Send {
                    channel_id,
                    client_id,
                    content,
                    ..
                } => {
                    text(&content)?;
                    let existing: Vec<serde_json::Value> = query(
                        c,
                        "SELECT jsonb_build_object('id',id,'guild_id',guild_id,'channel_id',channel_id,'request_hash',request_hash) AS data FROM messages WHERE author_id=$1::uuid AND client_id=$2::uuid",
                        &[&actor.to_string(), &client_id.to_string()],
                    )?;
                    let hash = auth::digest(&content);
                    if let Some(old) = existing.first() {
                        if old["guild_id"] != guild.to_string()
                            || old["channel_id"] != channel_id.to_string()
                            || old["request_hash"] != hash
                        {
                            return Err(Failure::Conflict);
                        }
                        let id = serde_json::from_value(old["id"].clone())
                            .map_err(|_| Failure::Unavailable)?;
                        return Ok(ChatResponse::Message {
                            message: message(c, id)?,
                        });
                    }
                    let id = MessageId::from_uuid(Uuid::new_v4());
                    // Channel writes share the channel lock. Make timestamp ordering monotonic even if the clock steps back.
                    let mentions = mentions(c, guild, &content)?;
                    let inserted = execute(
                        c,
                        "INSERT INTO messages(id,guild_id,channel_id,author_id,client_id,request_hash,content,mentions,created_at) VALUES($1::uuid,$2::uuid,$3::uuid,$4::uuid,$5::uuid,$6,$7,$8::jsonb,GREATEST(clock_timestamp(),COALESCE((SELECT max(created_at)+interval '1 microsecond' FROM messages WHERE guild_id=$2::uuid AND channel_id=$3::uuid),clock_timestamp()))) ON CONFLICT(author_id,client_id) DO NOTHING",
                        &[
                            &id.to_string(),
                            &guild.to_string(),
                            &channel_id.to_string(),
                            &actor.to_string(),
                            &client_id.to_string(),
                            &hash,
                            &content,
                            &mentions,
                        ],
                    )?;
                    if inserted == 0 {
                        // A simultaneous reuse in another channel/guild bypasses our
                        // channel lock. The unique author/client key chooses one winner.
                        return Err(Failure::Conflict);
                    }
                    event(c, guild, channel_id, id)?;
                    Ok(ChatResponse::Message {
                        message: message(c, id)?,
                    })
                }
                ChatRequest::Edit {
                    channel_id,
                    message_id,
                    revision,
                    content,
                    ..
                } => {
                    let old = message(c, message_id)?;
                    scope(&old, guild, channel_id, revision)?;
                    if old.author_id != Some(actor)
                        || !effective(state, actor, Some(channel_id))
                            .contains(&Permission::EditOwnMessages)
                    {
                        return Err(Failure::Forbidden);
                    }
                    text(&content)?;
                    let mentions = mentions(c, guild, &content)?;
                    execute(
                        c,
                        "UPDATE messages SET content=$2,mentions=$3::jsonb,edited_at=clock_timestamp(),revision=revision+1 WHERE id=$1::uuid",
                        &[&message_id.to_string(), &content, &mentions],
                    )?;
                    event(c, guild, channel_id, message_id)?;
                    Ok(ChatResponse::Message {
                        message: message(c, message_id)?,
                    })
                }
                ChatRequest::Delete {
                    channel_id,
                    message_id,
                    revision,
                    ..
                } => {
                    let old = message(c, message_id)?;
                    scope(&old, guild, channel_id, revision)?;
                    let p = effective(state, actor, Some(channel_id));
                    if !(p.contains(&Permission::ManageMessages)
                        || (old.author_id == Some(actor)
                            && p.contains(&Permission::DeleteOwnMessages)))
                    {
                        return Err(Failure::Forbidden);
                    }
                    execute(
                        c,
                        "UPDATE messages SET deleted=TRUE,content='',mentions='[]',edited_at=clock_timestamp(),revision=revision+1 WHERE id=$1::uuid",
                        &[&message_id.to_string()],
                    )?;
                    event(c, guild, channel_id, message_id)?;
                    Ok(ChatResponse::Message {
                        message: message(c, message_id)?,
                    })
                }
                ChatRequest::Read {
                    channel_id,
                    through,
                    ..
                } => {
                    if through < 0 {
                        return Err(Failure::Invalid("Invalid read position"));
                    }
                    execute(
                        c,
                        "INSERT INTO channel_reads(guild_id,channel_id,account_id,through) VALUES($1::uuid,$2::uuid,$3::uuid,LEAST($4::bigint,COALESCE((SELECT max(sequence) FROM messages WHERE guild_id=$1::uuid AND channel_id=$2::uuid),0))) ON CONFLICT(guild_id,channel_id,account_id) DO UPDATE SET through=GREATEST(channel_reads.through,EXCLUDED.through)",
                        &[
                            &guild.to_string(),
                            &channel_id.to_string(),
                            &actor.to_string(),
                            &through.to_string(),
                        ],
                    )?;
                    Ok(ChatResponse::Done)
                }
                ChatRequest::Unread { .. } => Ok(ChatResponse::Unread {
                    channels: unread(c, guild, actor, state)?,
                }),
            }
        },
    )
}
fn scope(
    message: &ChatMessage,
    guild: GuildId,
    channel: ChannelId,
    revision: i32,
) -> Result<(), Failure> {
    if message.guild_id != guild || message.channel_id != channel {
        return Err(Failure::Forbidden);
    }
    if message.revision != revision || message.deleted {
        return Err(Failure::Conflict);
    }
    Ok(())
}
fn event(
    c: &mut PgConnection,
    guild: GuildId,
    channel: ChannelId,
    id: MessageId,
) -> Result<(), Failure> {
    execute(
        c,
        "INSERT INTO message_events(guild_id,channel_id,message_id) VALUES($1::uuid,$2::uuid,$3::uuid)",
        &[&guild.to_string(), &channel.to_string(), &id.to_string()],
    )?;
    Ok(())
}

pub(super) struct Poll {
    pub events: Vec<(i64, ChatMessage)>,
    pub members: Vec<OnlineMember>,
}
pub(super) fn poll(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: ChannelId,
    cursor: i64,
    connection_id: Uuid,
    typing: bool,
) -> Result<Poll, Failure> {
    checked(
        pool,
        token,
        guild,
        Some(channel),
        Some(Permission::ReadHistory),
        |c, actor, state| poll_checked(c, actor, state, channel, cursor, connection_id, typing),
    )
}
fn poll_checked(
    c: &mut PgConnection,
    actor: AccountId,
    state: &GuildState,
    channel: ChannelId,
    cursor: i64,
    connection_id: Uuid,
    typing: bool,
) -> Result<Poll, Failure> {
    let guild = state.guild.id;
    check_channel(state, actor, Some(channel), Some(Permission::ReadHistory))?;

    if typing && !effective(state, actor, Some(channel)).contains(&Permission::SendMessages) {
        return Err(Failure::Forbidden);
    }
    execute(c, "DELETE FROM chat_presence WHERE expires_at<now()", &[])?;
    execute(
        c,
        "INSERT INTO chat_presence(id,guild_id,channel_id,account_id) VALUES($1::uuid,$2::uuid,$3::uuid,$4::uuid) ON CONFLICT(id) DO UPDATE SET expires_at=now()+interval '35 seconds'",
        &[
            &connection_id.to_string(),
            &guild.to_string(),
            &channel.to_string(),
            &actor.to_string(),
        ],
    )?;
    if typing {
        execute(
            c,
            "UPDATE chat_presence SET typing_until=now()+interval '4 seconds' WHERE id=$1::uuid",
            &[&connection_id.to_string()],
        )?;
    }
    let events: Vec<(i64, ChatMessage)> = query(
        c,
        "SELECT jsonb_build_array(e.sequence,to_jsonb(m)||jsonb_build_object('username',COALESCE(a.username,'Deleted account'),'display_name',COALESCE(NULLIF(a.display_name,''),a.username,'Deleted account'))) AS data FROM message_events e JOIN messages m ON m.id=e.message_id LEFT JOIN accounts a ON a.id=m.author_id WHERE e.guild_id=$1::uuid AND e.channel_id=$2::uuid AND e.sequence>$3::bigint ORDER BY e.sequence LIMIT 100",
        &[
            &guild.to_string(),
            &channel.to_string(),
            &cursor.to_string(),
        ],
    )?;
    let members: Vec<OnlineMember> = query(
        c,
        "SELECT jsonb_build_object('account_id',p.account_id,'username',a.username,'display_name',a.display_name,'typing',bool_or(p.typing_until>now())) AS data FROM chat_presence p JOIN accounts a ON a.id=p.account_id WHERE p.guild_id=$1::uuid AND p.channel_id=$2::uuid AND p.expires_at>now() GROUP BY p.account_id,a.username,a.display_name ORDER BY a.username",
        &[&guild.to_string(), &channel.to_string()],
    )?;
    // Presence has its own bounded set of actors. Evaluate only those
    // members and the selected channel, retaining per-recipient privacy.
    let accounts: Vec<_> = members
        .iter()
        .filter(|m| m.account_id != actor)
        .map(|m| m.account_id)
        .collect();
    let presence = if accounts.is_empty() {
        None
    } else {
        Some(load(c, guild, &accounts, Some(channel))?)
    };
    Ok(Poll {
        events,
        members: members
            .into_iter()
            .filter(|m| {
                // The caller's ViewChannel grant was already checked above.
                m.account_id == actor
                    || presence.as_ref().is_some_and(|state| {
                        effective(state, m.account_id, Some(channel))
                            .contains(&Permission::ViewChannel)
                    })
            })
            .collect(),
    })
}

pub(super) fn refresh(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: Option<ChannelId>,
    cursor: i64,
    connection_id: Uuid,
    typing: bool,
) -> Result<(Vec<Unread>, Option<Poll>), Failure> {
    checked(pool, token, guild, None, None, |c, actor, state| {
        let unread = unread(c, guild, actor, state)?;
        let poll = channel
            .map(|ch| poll_checked(c, actor, state, ch, cursor, connection_id, typing))
            .transpose()?;
        Ok((unread, poll))
    })
}

pub(super) fn cleanup(pool: &DbPool, id: Uuid) {
    if let Ok(mut c) = connection(pool) {
        let _ = execute(
            &mut c,
            "DELETE FROM chat_presence WHERE id=$1::uuid",
            &[&id.to_string()],
        );
    }
}

pub(super) fn unread(
    c: &mut PgConnection,
    guild: GuildId,
    actor: AccountId,
    state: &GuildState,
) -> Result<Vec<Unread>, Failure> {
    // Authorize before touching message history, using the state loaded under
    // the guild lock by checked(). Never scan inaccessible channels.
    let channels: Vec<_> = state
        .channels
        .iter()
        .filter(|ch| {
            let p = effective(state, actor, Some(ch.id));
            ch.kind == ChannelKind::Text
                && p.contains(&Permission::ViewChannel)
                && p.contains(&Permission::ReadHistory)
        })
        .map(|ch| ch.id.to_string())
        .collect();
    if channels.is_empty() {
        return Ok(Vec::new());
    }
    query(
        c,
        include_str!("unread.sql"),
        &[
            &guild.to_string(),
            &actor.to_string(),
            &format!("{{{}}}", channels.join(",")),
        ],
    )
}

pub(super) fn maintain(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: Option<ChannelId>,
    connection_id: Uuid,
) -> Result<(bool, thiscord_shared::permissions::Permissions), Failure> {
    checked(
        pool,
        token,
        guild,
        channel,
        Some(Permission::ReadHistory),
        |c, actor, state| {
            let removed = execute(c, "DELETE FROM chat_presence WHERE expires_at<now()", &[])?;
            execute(
                c,
                "UPDATE chat_presence SET expires_at=now()+interval '35 seconds' WHERE id=$1::uuid",
                &[&connection_id.to_string()],
            )?;
            Ok((removed > 0, effective(state, actor, channel)))
        },
    )
}
