//! Rich message operations run inside the existing authenticated guild/channel transaction.
use crate::auth::{
    Failure,
    store::{execute, query},
};
use diesel::PgConnection;
use thiscord_shared::{AccountId, ChannelId, GuildId, MessageId, chat::*, pagination::PageCursor};

pub(super) fn one(
    c: &mut PgConnection,
    actor: AccountId,
    mut message: ChatMessage,
) -> Result<ChatMessage, Failure> {
    hydrate(c, actor, std::slice::from_mut(&mut message))?;
    Ok(message)
}

pub(super) fn hydrate(
    c: &mut PgConnection,
    actor: AccountId,
    messages: &mut [ChatMessage],
) -> Result<(), Failure> {
    if messages.is_empty() {
        return Ok(());
    }
    let ids: Vec<_> = messages.iter().map(|m| m.id).collect();
    let values: Vec<(MessageId, Option<ReplyPreview>, Vec<Reaction>)> = query(
        c,
        r#"
        SELECT jsonb_build_array(m.id,
          CASE WHEN m.deleted OR m.reply_to IS NULL THEN NULL ELSE
            jsonb_build_object('id',m.reply_to,'preview',CASE WHEN t.id IS NULL OR t.deleted THEN NULL ELSE left(t.content,160) END) END,
          CASE WHEN m.deleted THEN '[]'::jsonb ELSE COALESCE((
            SELECT jsonb_agg(r ORDER BY r.emoji) FROM (
              SELECT emoji,count(*) AS count,bool_or(account_id=$2::uuid) AS mine
              FROM message_reactions WHERE message_id=m.id GROUP BY emoji
            ) r), '[]'::jsonb) END) AS data
        FROM messages m LEFT JOIN messages t ON t.id=m.reply_to AND t.guild_id=m.guild_id AND t.channel_id=m.channel_id
        WHERE m.id IN (SELECT value::uuid FROM jsonb_array_elements_text($1::jsonb))
    "#,
        &[
            &serde_json::to_string(&ids).map_err(|_| Failure::Unavailable)?,
            &actor.to_string(),
        ],
    )?;
    let values: std::collections::HashMap<_, _> = values
        .into_iter()
        .map(|(id, reply, reactions)| (id, (reply, reactions)))
        .collect();
    for message in messages {
        if let Some((reply, reactions)) = values.get(&message.id) {
            message.reply = reply.clone();
            message.reactions = reactions.clone();
        }
    }
    Ok(())
}

pub(super) fn react(
    c: &mut PgConnection,
    actor: AccountId,
    guild: GuildId,
    channel: ChannelId,
    id: MessageId,
    emoji: &str,
    present: bool,
) -> Result<bool, Failure> {
    if !REACTION_EMOJI.contains(&emoji) {
        return Err(Failure::Invalid("Unsupported reaction"));
    }
    let exists: Vec<bool> = query(
        c,
        "SELECT to_jsonb(TRUE) AS data FROM messages WHERE id=$1::uuid AND guild_id=$2::uuid AND channel_id=$3::uuid AND NOT deleted",
        &[&id.to_string(), &guild.to_string(), &channel.to_string()],
    )?;
    if exists.is_empty() {
        return Err(Failure::Forbidden);
    }
    let changed = if present {
        execute(
            c,
            "INSERT INTO message_reactions(message_id,account_id,emoji) VALUES($1::uuid,$2::uuid,$3) ON CONFLICT DO NOTHING",
            &[&id.to_string(), &actor.to_string(), emoji],
        )?
    } else {
        execute(
            c,
            "DELETE FROM message_reactions WHERE message_id=$1::uuid AND account_id=$2::uuid AND emoji=$3",
            &[&id.to_string(), &actor.to_string(), emoji],
        )?
    };
    if changed > 0 {
        execute(
            c,
            "UPDATE messages SET revision=revision+1 WHERE id=$1::uuid",
            &[&id.to_string()],
        )?;
    }
    Ok(changed > 0)
}
pub(super) fn pin(
    c: &mut PgConnection,
    guild: GuildId,
    channel: ChannelId,
    id: MessageId,
    pinned: bool,
) -> Result<(), Failure> {
    if pinned {
        let count: i64=query(c,"SELECT to_jsonb(count(*)) AS data FROM messages WHERE guild_id=$1::uuid AND channel_id=$2::uuid AND pinned AND NOT deleted", &[&guild.to_string(),&channel.to_string()])?.pop().unwrap_or(0);
        if count >= MAX_CHANNEL_PINS as i64 {
            return Err(Failure::Invalid(
                "A channel can have at most 50 pinned messages",
            ));
        }
    }
    execute(
        c,
        "UPDATE messages SET pinned=$2::boolean,revision=revision+1 WHERE id=$1::uuid",
        &[&id.to_string(), &pinned.to_string()],
    )?;
    Ok(())
}
pub(super) fn pins(
    c: &mut PgConnection,
    actor: AccountId,
    guild: GuildId,
    channel: ChannelId,
) -> Result<Vec<ChatMessage>, Failure> {
    let mut rows = query(
        c,
        "SELECT to_jsonb(m)||jsonb_build_object('username',COALESCE(a.username,'Deleted account'),'display_name',COALESCE(a.display_name,'')) AS data FROM messages m LEFT JOIN accounts a ON a.id=m.author_id WHERE guild_id=$1::uuid AND channel_id=$2::uuid AND pinned AND NOT deleted ORDER BY m.created_at DESC,m.id DESC LIMIT 50",
        &[&guild.to_string(), &channel.to_string()],
    )?;
    hydrate(c, actor, &mut rows)?;
    Ok(rows)
}
pub(super) fn context(
    c: &mut PgConnection,
    actor: AccountId,
    guild: GuildId,
    channel: ChannelId,
    target: MessageId,
) -> Result<History, Failure> {
    execute(
        c,
        "SELECT id FROM channels WHERE guild_id=$1::uuid AND id=$2::uuid FOR SHARE",
        &[&guild.to_string(), &channel.to_string()],
    )?;
    let target = super::store::message(c, target)?;
    if target.guild_id != guild || target.channel_id != channel || target.deleted {
        return Err(Failure::Forbidden);
    }
    let mut rows: Vec<ChatMessage> = query(
        c,
        r#"
        WITH nearby AS (
          (SELECT * FROM messages WHERE guild_id=$1::uuid AND channel_id=$2::uuid AND (created_at,id)<($3::timestamptz,$4::uuid) ORDER BY created_at DESC,id DESC LIMIT 25)
          UNION ALL
          (SELECT * FROM messages WHERE guild_id=$1::uuid AND channel_id=$2::uuid AND (created_at,id)>=($3::timestamptz,$4::uuid) ORDER BY created_at,id LIMIT 26)
        ) SELECT to_jsonb(m)||jsonb_build_object('username',COALESCE(a.username,'Deleted account'),'display_name',COALESCE(a.display_name,'')) AS data FROM nearby m LEFT JOIN accounts a ON a.id=m.author_id ORDER BY m.created_at,m.id
    "#,
        &[
            &guild.to_string(),
            &channel.to_string(),
            &target.created_at.to_rfc3339(),
            &target.id.to_string(),
        ],
    )?;
    hydrate(c, actor, &mut rows)?;
    let older = rows
        .first()
        .map(|m| PageCursor::new(m.created_at, m.id.as_uuid()));
    Ok(History {
        messages: rows,
        older,
        event_cursor: 0,
    })
}
