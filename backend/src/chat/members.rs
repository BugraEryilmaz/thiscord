//! Guild member lists and public member profiles.
//!
//! The database snapshot is read under the subscription's authorization check;
//! online flags come from in-memory account presence (the friends socket
//! registry) and are refreshed without database work. Presence and custom
//! status are withheld across a block in either direction.
use super::store::checked;
use crate::{
    auth::{Failure, store::query},
    db::DbPool,
};
use diesel::PgConnection;
use serde::Deserialize;
use std::time::Duration;
use thiscord_shared::{
    AccountId, GuildId,
    chat::{GuildMember, MAX_MEMBER_LIST, MemberProfile},
};

#[derive(Deserialize)]
struct Row {
    member: GuildMember,
    /// A block exists between the viewer and this member (either direction).
    hidden: bool,
}

/// Authorized member rows for one viewer, without live presence.
pub(super) struct MemberList {
    rows: Vec<Row>,
}

impl MemberList {
    /// Visible online members, in list order.
    pub(super) fn online(&self, online: impl Fn(AccountId) -> bool) -> Vec<AccountId> {
        self.rows
            .iter()
            .filter(|row| !row.hidden && online(row.member.account_id))
            .map(|row| row.member.account_id)
            .collect()
    }
    /// The full list with `online` applied.
    pub(super) fn members(&self, online: &[AccountId]) -> Vec<GuildMember> {
        self.rows
            .iter()
            .map(|row| {
                let mut member = row.member.clone();
                member.online = online.contains(&member.account_id);
                member
            })
            .collect()
    }
    /// Time until the earliest listed custom status expires, when a refresh
    /// must remove it although nothing was written.
    pub(super) fn expiry(&self, now: thiscord_shared::Timestamp) -> Option<Duration> {
        self.rows
            .iter()
            .filter_map(|row| row.member.custom_status.as_ref()?.expires_at)
            .min()
            .map(|at| (at - now).to_std().unwrap_or_default() + Duration::from_millis(50))
    }
}

/// Every member of the guild (at most [`MAX_MEMBER_LIST`]), sorted by display
/// name. Requires current membership; runs on a blocking worker.
pub(super) fn snapshot(pool: &DbPool, token: &str, guild: GuildId) -> Result<MemberList, Failure> {
    checked(pool, token, guild, None, None, |c, actor, _| {
        Ok(MemberList {
            rows: query(
                c,
                "SELECT jsonb_build_object('member',jsonb_strip_nulls(jsonb_build_object('account_id',a.id,'username',a.username,'display_name',a.display_name,'avatar_id',v.id,'online',false,'owner',a.id=g.owner,'bot',a.is_bot,'custom_status',CASE WHEN s.account_id IS NULL THEN NULL ELSE jsonb_build_object('emoji',s.emoji,'text',s.status_text,'expires_at',s.expires_at) END)),'hidden',h.hidden) AS data FROM guild_members m JOIN guilds g ON g.id=m.guild_id JOIN accounts a ON a.id=m.account_id LEFT JOIN account_avatars v ON v.account_id=a.id CROSS JOIN LATERAL (SELECT EXISTS(SELECT 1 FROM account_blocks b WHERE (b.blocker_id=$2::uuid AND b.blocked_id=a.id) OR (b.blocker_id=a.id AND b.blocked_id=$2::uuid)) AS hidden) h LEFT JOIN account_custom_statuses s ON s.account_id=a.id AND NOT h.hidden AND (s.expires_at IS NULL OR s.expires_at>now()) WHERE m.guild_id=$1::uuid ORDER BY lower(COALESCE(NULLIF(a.display_name,''),a.username)),a.username,a.id LIMIT $3::integer",
                &[
                    &guild.to_string(),
                    &actor.to_string(),
                    &MAX_MEMBER_LIST.to_string(),
                ],
            )?,
        })
    })
}

/// Public profile of a current member. The caller has already verified the
/// actor's membership inside the same guild-locked transaction.
pub(super) fn profile(
    c: &mut PgConnection,
    guild: GuildId,
    actor: AccountId,
    target: AccountId,
) -> Result<MemberProfile, Failure> {
    query(
        c,
        "SELECT jsonb_strip_nulls(jsonb_build_object('account_id',a.id,'username',a.username,'display_name',a.display_name,'bio',a.bio,'avatar_id',v.id,'owner',a.id=g.owner,'bot',a.is_bot,'custom_status',CASE WHEN s.account_id IS NULL THEN NULL ELSE jsonb_build_object('emoji',s.emoji,'text',s.status_text,'expires_at',s.expires_at) END)) AS data FROM guild_members m JOIN guilds g ON g.id=m.guild_id JOIN accounts a ON a.id=m.account_id LEFT JOIN account_avatars v ON v.account_id=a.id LEFT JOIN account_custom_statuses s ON s.account_id=a.id AND (s.expires_at IS NULL OR s.expires_at>now()) AND NOT EXISTS(SELECT 1 FROM account_blocks b WHERE (b.blocker_id=$3::uuid AND b.blocked_id=a.id) OR (b.blocker_id=a.id AND b.blocked_id=$3::uuid)) WHERE m.guild_id=$1::uuid AND m.account_id=$2::uuid",
        &[&guild.to_string(), &target.to_string(), &actor.to_string()],
    )?
    .pop()
    .ok_or(Failure::NotFound)
}

/// Guilds whose member lists show this account (profile/status changes).
pub(crate) fn guilds_of(c: &mut PgConnection, account: AccountId) -> Result<Vec<GuildId>, Failure> {
    query(
        c,
        "SELECT to_jsonb(guild_id) AS data FROM guild_members WHERE account_id=$1::uuid",
        &[&account.to_string()],
    )
}

/// Call after commit. Wakes member-list subscribers in these guilds to re-read
/// their authorized lists; carries no data or authorization.
pub(crate) fn changed(guilds: impl IntoIterator<Item = GuildId>) {
    for guild in guilds {
        super::notifications::notifications().publish(super::notifications::Change::Members(guild));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn list(rows: serde_json::Value) -> MemberList {
        MemberList {
            rows: serde_json::from_value::<Vec<Row>>(rows).unwrap(),
        }
    }

    #[test]
    fn presence_and_expiry_respect_hidden_members_and_order() {
        let a = AccountId::from_uuid(uuid::Uuid::from_u128(1));
        let b = AccountId::from_uuid(uuid::Uuid::from_u128(2));
        let c = AccountId::from_uuid(uuid::Uuid::from_u128(3));
        let now: thiscord_shared::Timestamp = "2026-11-03T10:00:00Z".parse().unwrap();
        let members = list(json!([
            {"member":{"account_id":a,"username":"ada","display_name":"Ada","online":false,
                "custom_status":{"text":"Later","expires_at":"2026-11-03T11:00:00Z"}},"hidden":false},
            {"member":{"account_id":b,"username":"blocked","display_name":"B","online":false},"hidden":true},
            {"member":{"account_id":c,"username":"cy","display_name":"Cy","online":false,
                "custom_status":{"emoji":"🎮","text":"","expires_at":"2026-11-03T10:30:00Z"}},"hidden":false},
        ]));
        // A blocked member never appears online, even with a live socket.
        let online = members.online(|_| true);
        assert_eq!(online, vec![a, c]);
        let view = members.members(&online);
        assert_eq!(
            view.iter()
                .map(|m| (m.account_id, m.online))
                .collect::<Vec<_>>(),
            vec![(a, true), (b, false), (c, true)]
        );
        assert_eq!(
            members.expiry(now),
            Some(Duration::from_secs(30 * 60) + Duration::from_millis(50))
        );
        // Already expired (clock moved on): refresh immediately.
        let later: thiscord_shared::Timestamp = "2026-11-03T12:00:00Z".parse().unwrap();
        assert_eq!(members.expiry(later), Some(Duration::from_millis(50)));
        assert_eq!(list(json!([])).expiry(now), None);
    }
}
