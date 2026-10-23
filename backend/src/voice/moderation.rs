//! Live membership hints only; guild permissions and revisions stay transactional.
use super::*;
use thiscord_shared::AccountId;

struct Live {
    guild: GuildId,
    channel: ChannelId,
    account: AccountId,
    source: uuid::Uuid,
    active: Arc<AtomicBool>,
    access: Arc<access::Access>,
    generation: Arc<AtomicU64>,
}
static LIVE: OnceLock<std::sync::Mutex<Vec<Live>>> = OnceLock::new();
pub(super) fn register(room: &Room, member: &Member) {
    let Some((guild, channel)) = room.notification_target else {
        return;
    };
    let mut live = LIVE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    live.retain(|m| m.active.load(Ordering::Acquire));
    live.push(Live {
        guild,
        channel,
        account: member.info.account_id,
        source: member.source_id,
        active: member.active.clone(),
        access: member.access.clone(),
        generation: member.generation.clone(),
    });
}
pub(super) fn remove(source: uuid::Uuid) {
    LIVE.get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .retain(|m| m.source != source && m.active.load(Ordering::Acquire));
}
pub(crate) fn check_move(
    guild: GuildId,
    source: ChannelId,
    destination: ChannelId,
    account: AccountId,
) -> Result<(), Failure> {
    let live = LIVE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| Failure::Unavailable)?;
    let valid = |m: &&Live| m.guild == guild && m.active.load(Ordering::Acquire);
    let current = live
        .iter()
        .filter(valid)
        .find(|m| m.channel == source && m.account == account)
        .ok_or(Failure::Conflict)?;
    let epoch = current.generation.load(Ordering::Acquire);
    let _permit = current
        .access
        .packet(epoch, epoch)
        .ok_or(Failure::Conflict)?;
    if live
        .iter()
        .filter(valid)
        .filter(|m| m.channel == destination && m.account != account)
        .count()
        >= ROOM_CAPACITY
    {
        return Err(Failure::Capacity("Destination voice channel is full"));
    }
    Ok(())
}

/// Called only after an old connection loses its revision. Reauthenticate and
/// recheck both destination access and exact move revision before disclosing it.
pub(super) fn destination(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    source: ChannelId,
    revision: i64,
) -> Option<ChannelId> {
    use crate::auth::store::{self as auth, connection, execute, query};
    use diesel::Connection;
    let mut c = connection(pool).ok()?;
    let session = auth::authenticate_actor_retained(&mut c, token).ok()?;
    session.require_guild(guild).ok()?;
    c.transaction::<_,Failure,_>(|c|{
        if session.bot_guild.is_none() { auth::read_session(c,token,&session)?; }
        execute(c,"SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE", &[&guild.to_string()])?;
        // Bot deletion locks its guild before deleting the backing account.
        if session.bot_guild.is_some() { auth::read_session(c,token,&session)?; }
        let destination:ChannelId=query(c,"SELECT to_jsonb(move_destination) AS data FROM guild_moderation WHERE guild_id=$1::uuid AND account_id=$2::uuid AND move_source=$3::uuid AND move_revision=voice_revision AND voice_revision=$4::bigint+1 AND NOT banned", &[&guild.to_string(),&session.account_id.to_string(),&source.to_string(),&revision.to_string()])?.pop().ok_or(Failure::Forbidden)?;
        let state=crate::permissions::authorization::load(c,guild,&[session.account_id],Some(destination))?;
        let permissions=crate::permissions::evaluator::effective(&state,session.account_id,Some(destination));
        if !state.channels.iter().any(|ch|ch.id==destination && ch.kind==thiscord_shared::permissions::ChannelKind::Voice) || !permissions.contains(&thiscord_shared::permissions::Permission::JoinVoice) || !permissions.contains(&thiscord_shared::permissions::Permission::ViewChannel) {return Err(Failure::Forbidden);}
        Ok(destination)
    }).ok()
}
