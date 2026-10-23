use crate::{
    access::{self, Scope},
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
    permissions::{
        authorization::load,
        evaluator::{effective, rank},
    },
};
use diesel::{Connection, PgConnection};
use thiscord_shared::{AccountId, GuildId, SessionId, bots::*, permissions::Permission};
use uuid::Uuid;

pub(super) fn info(c: &mut PgConnection, account: AccountId) -> Result<BotInfo, Failure> {
    query(c, "SELECT jsonb_build_object('account_id',b.account_id,'guild_id',b.guild_id,'name',a.username) AS data FROM scripting_bots b JOIN accounts a ON a.id=b.account_id WHERE b.account_id=$1::uuid", &[&account.to_string()])?.pop().ok_or(Failure::Forbidden)
}
fn credential(c: &mut PgConnection, account: AccountId) -> Result<String, Failure> {
    // Rotation replaces all sessions rather than accumulating inactive secrets.
    execute(
        c,
        "DELETE FROM sessions WHERE account_id=$1::uuid",
        &[&account.to_string()],
    )?;
    let session = SessionId::from_uuid(Uuid::new_v4());
    let token = format!("bot_{}", auth::secret());
    execute(
        c,
        "INSERT INTO sessions(id,account_id,device,expires_at) VALUES($1::uuid,$2::uuid,'Scripting bot','infinity')",
        &[&session.to_string(), &account.to_string()],
    )?;
    execute(
        c,
        "INSERT INTO session_tokens(token_hash,session_id) VALUES($1,$2::uuid)",
        &[&auth::digest(&token), &session.to_string()],
    )?;
    Ok(token)
}

/// Possession of a bot credential delegates all of its effective capabilities,
/// including member/channel overrides. Hierarchy alone does not bound those.
fn grantable(
    c: &mut PgConnection,
    guild: GuildId,
    actor: AccountId,
    bot: AccountId,
) -> Result<(), Failure> {
    let mut state = load(c, guild, &[actor, bot], None)?;
    if state.guild.owner == actor {
        return Ok(());
    }
    // A timeout is temporary. Issuing a durable credential delegates the bot's
    // grants after it expires too; retain the issuer's current restrictions.
    if let Some(member) = state
        .members
        .iter_mut()
        .find(|member| member.account_id == bot)
    {
        member.timeout_until = None;
    }
    for channel in
        std::iter::once(None).chain(state.channels.iter().map(|channel| Some(channel.id)))
    {
        if !effective(&state, bot, channel).is_subset(&effective(&state, actor, channel)) {
            return Err(Failure::Forbidden);
        }
    }
    Ok(())
}
pub(super) fn dispatch(
    pool: &DbPool,
    token: &str,
    command: BotRequest,
) -> Result<BotResponse, Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    drop(c);
    auth::rate_limit(
        pool,
        &format!("bots:{}", session.account_id),
        None,
        !matches!(command, BotRequest::List { .. }),
    )?;
    let mut c = connection(pool)?;
    let guild = match &command {
        BotRequest::List { guild_id }
        | BotRequest::Create { guild_id, .. }
        | BotRequest::Rotate { guild_id, .. }
        | BotRequest::Delete { guild_id, .. } => *guild_id,
    };
    let mut mutations = Vec::new();
    let result = c.transaction(|c| {
        auth::read_session(c, token, &session)?;
        execute(c, "SELECT singleton FROM instance FOR SHARE", &[])?;
        execute(c, "SELECT id FROM guilds WHERE id=$1::uuid FOR UPDATE", &[&guild.to_string()])?;
        // Include the target to enforce hierarchy before issuing its credentials.
        let target = match &command { BotRequest::Rotate{account_id,..} | BotRequest::Delete{account_id,..} => Some(*account_id), _ => None };
        let mut accounts = vec![session.account_id]; accounts.extend(target);
        let state = load(c, guild, &accounts, None)?;
        if !state.members.iter().any(|m| m.account_id == session.account_id)
            || !effective(&state, session.account_id, None).contains(&Permission::ManageGuild) {
            return Err(Failure::Forbidden);
        }
        let revision = state.guild.revision;
        let expected = match &command { BotRequest::List{..} => None, BotRequest::Create{revision,..} | BotRequest::Rotate{revision,..} | BotRequest::Delete{revision,..} => Some(*revision) };
        if expected.is_some_and(|r| r != revision) { return Err(Failure::Conflict); }
        if let Some(target) = target {
            if info(c, target)?.guild_id != guild { return Err(Failure::Forbidden); }
            if session.account_id != state.guild.owner && rank(&state, session.account_id) <= rank(&state, target) { return Err(Failure::Forbidden); }
        }
        let response = match command {
            BotRequest::List { .. } => {
                let bots = query(c, "SELECT jsonb_build_object('account_id',b.account_id,'guild_id',b.guild_id,'name',a.username) AS data FROM scripting_bots b JOIN accounts a ON a.id=b.account_id WHERE b.guild_id=$1::uuid ORDER BY a.username LIMIT 10", &[&guild.to_string()])?;
                return Ok(BotResponse::Bots { bots, revision });
            }
            BotRequest::Create { name, .. } => {
                if !(3..=32).contains(&name.len()) || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_') { return Err(Failure::Invalid("Bot name must contain 3 to 32 lowercase letters, digits or underscores")); }
                let count: i64 = query(c, "SELECT to_jsonb(count(*)) AS data FROM scripting_bots WHERE guild_id=$1::uuid", &[&guild.to_string()])?[0];
                if count >= MAX_BOTS_PER_GUILD as i64 { return Err(Failure::Capacity("Server bot limit reached")); }
                let count: i64 = query(c, "SELECT to_jsonb(count(*)) AS data FROM guild_members WHERE guild_id=$1::uuid", &[&guild.to_string()])?[0];
                if count >= i64::from(crate::permissions::store::capacity(c, state.guild.owner)?.limits.members) { return Err(Failure::Capacity("Server member limit reached")); }
                let account_id = AccountId::from_uuid(Uuid::new_v4());
                execute(c, "INSERT INTO accounts(id,username,email,display_name,is_bot) VALUES($1::uuid,$2,$1||'@bots.invalid',$2||' [bot]',TRUE)", &[&account_id.to_string(), &name])?;
                execute(c, "INSERT INTO scripting_bots(account_id,guild_id,creator_id) VALUES($1::uuid,$2::uuid,$3::uuid)", &[&account_id.to_string(), &guild.to_string(), &session.account_id.to_string()])?;
                execute(c, "INSERT INTO guild_members(guild_id,account_id) VALUES($1::uuid,$2::uuid)", &[&guild.to_string(), &account_id.to_string()])?;
                grantable(c,guild,session.account_id,account_id)?;
                BotResponse::Credential { bot: BotInfo { account_id, guild_id: guild, name }, token: credential(c, account_id)?, revision: revision + 1 }
            }
            BotRequest::Rotate { account_id, .. } => {
                if !state.members.iter().any(|m| m.account_id==account_id) { return Err(Failure::Forbidden); }
                let bot = info(c, account_id)?;
                grantable(c,guild,session.account_id,account_id)?;
                let token = credential(c, account_id)?;
                mutations.push(access::pause(Scope::Account(account_id)));
                BotResponse::Credential { bot, token, revision: revision + 1 }
            }
            BotRequest::Delete { account_id, .. } => {
                execute(c, "DELETE FROM accounts WHERE id=$1::uuid AND is_bot", &[&account_id.to_string()])?;
                mutations.push(access::pause(Scope::Account(account_id)));
                BotResponse::Done { revision: revision + 1 }
            }
        };
        execute(c, "UPDATE guilds SET revision=revision+1 WHERE id=$1::uuid", &[&guild.to_string()])?;
        // Rotation changes only this account's authority; unrelated guild media
        // stays active. Membership creation/deletion invalidates guild snapshots.
        if !matches!(response, BotResponse::Credential { ref bot, .. } if target == Some(bot.account_id)) {
            mutations.push(access::pause(Scope::Guild(guild)));
        }
        Ok(response)
    });
    access::complete(result, &mut mutations)
}

pub(super) fn checked(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channels: &[thiscord_shared::ChannelId],
) -> Result<(), Failure> {
    for channel in channels {
        crate::chat::store::checked(
            pool,
            token,
            guild,
            Some(*channel),
            Some(Permission::ReadHistory),
            |_, _, _| Ok(()),
        )?;
    }
    Ok(())
}

pub(super) fn channels(pool: &DbPool, token: &str) -> Result<ChannelsResponse, Failure> {
    let session = {
        let mut c = connection(pool)?;
        super::authenticate(&mut c, token)?
    };
    let guild = session.bot_guild.ok_or(Failure::Unauthorized)?;
    auth::rate_limit(
        pool,
        &format!("bot-channels:{}", session.account_id),
        None,
        false,
    )?;
    // Reauthenticate under the guild lock, including membership and token revocation.
    crate::chat::store::checked(pool, token, guild, None, None, |_, actor, state| {
        Ok(ChannelsResponse {
            channels: state
                .channels
                .iter()
                .filter(|channel| {
                    effective(state, actor, Some(channel.id)).contains(&Permission::ViewChannel)
                })
                .cloned()
                .collect(),
        })
    })
}
