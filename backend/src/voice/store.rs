use crate::{
    auth::{
        Failure,
        store::{self as auth, connection, execute, query},
    },
    db::DbPool,
    permissions::{authorization::load, evaluator::effective},
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
    // Join/resume ingress authenticates in access::identify; bot HTTP ingress
    // authenticates in bots::owner. This also runs on access-change wakeups,
    // which must reject a rotated token without revoking its replacement.
    let session = auth::authenticate_retained(&mut c, token)?;
    c.transaction(|c| {
        auth::read_session(c, token, &session)?;
        // Direct calls use a dedicated equal scope/room UUID pair. Authorization
        // always consults the accepted friendship and active invitation; no guild
        // role (including instance administration) grants private-call access.
        if guild.as_uuid() == channel.as_uuid() {
            return crate::friends::store::voice(c, session.account_id, channel);
        }
        execute(
            c,
            // Stable permission snapshot against guild writes, without serializing readers.
            "SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE",
            &[&guild.to_string()],
        )?;
        let state = load(c, guild, &[session.account_id], Some(channel))?;
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
            test_bot: false,
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
            microphone_epoch: 0,
            microphone_source_epoch: 0,
                        camera_active: false, camera_epoch: 0, screen_viewers: vec![], camera_viewers: vec![],
        }, revision))
    })
}
