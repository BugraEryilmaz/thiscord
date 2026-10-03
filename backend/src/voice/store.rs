use crate::{
    auth::{
        Failure,
        store::{self as auth, connection, execute},
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
) -> Result<Participant, Failure> {
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
        Ok(Participant {
            account_id: session.account_id,
            username: member.username.clone(),
            slot: 0,
            muted: false,
            deafened: false,
            can_speak: permissions.contains(&Permission::Speak),
        })
    })
}
