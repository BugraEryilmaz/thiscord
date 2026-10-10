use crate::{
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
    permissions::{evaluator::effective, store::load},
};
use diesel::Connection;
use thiscord_shared::{
    ChannelId, GuildId,
    permissions::{ChannelKind, Permission},
    voice::Participant,
};
pub(super) fn authorize(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: ChannelId,
) -> Result<(Participant, i64), Failure> {
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    c.transaction(|c| {
        auth::lock_session(c, token, &session)?;
        execute(
            c,
            // Stable permission snapshot against guild writes, without serializing readers.
            "SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE",
            &[&guild.to_string()],
        )?;
        let state = load(c, guild)?;
        let member = state
            .members
            .iter()
            .find(|m| m.account_id == session.account_id)
            .ok_or(Failure::Forbidden)?;
        if !state
            .channels
            .iter()
            .any(|c| c.id == channel && c.kind == ChannelKind::Voice)
        {
            return Err(Failure::Forbidden);
        }
        let permissions = effective(&state, session.account_id, Some(channel));
        if !permissions.contains(&Permission::ViewChannel)
            || !permissions.contains(&Permission::JoinVoice)
        {
            return Err(Failure::Forbidden);
        }
        let revision = query::<i64>(c,"SELECT to_jsonb(voice_revision) AS data FROM guild_moderation WHERE guild_id=$1::uuid AND account_id=$2::uuid", &[&guild.to_string(), &session.account_id.to_string()])?.pop().unwrap_or(0);
        Ok((Participant {
            account_id: session.account_id,
            username: member.username.clone(),
            display_name: member.display_name.clone(),
            avatar_id: query(c, "SELECT to_jsonb(id) AS data FROM account_avatars WHERE account_id=$1::uuid", &[&session.account_id.to_string()])?.pop(),
            slot: 0,
            muted: false,
            deafened: false,
            can_speak: permissions.contains(&Permission::Speak),
            sharing_screen: false,
            sharing_audio: false,
            screen_epoch: 0,
        }, revision))
    })
}
