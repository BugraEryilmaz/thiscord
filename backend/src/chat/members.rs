//! Guild member lists and public member profiles.
//!
//! The database snapshot is read under the subscription's authorization check;
//! online flags and live activities come from in-memory account presence (the
//! friends socket registry) and are refreshed without database work. Presence,
//! activities and custom status are withheld across a block in either direction.
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
    account::Activity,
    chat::{GuildMember, MAX_MEMBER_LIST, MemberActivity, MemberProfile},
};

#[derive(Deserialize)]
struct Row {
    member: GuildMember,
    /// A block exists between the viewer and this member (either direction).
    hidden: bool,
}

/// Live presence of the visible members of one list.
#[derive(Default, PartialEq)]
pub(super) struct Live {
    pub(super) online: Vec<AccountId>,
    /// Only for members in `online`.
    pub(super) activities: Vec<MemberActivity>,
}

impl Live {
    pub(super) fn apply(&self, members: &mut [GuildMember]) {
        for member in members {
            member.online = self.online.contains(&member.account_id);
            member.activity = self
                .activities
                .iter()
                .find(|a| a.account_id == member.account_id)
                .map(|a| a.activity.clone());
        }
    }
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
    /// Visible online members and their activities.
    pub(super) fn live(
        &self,
        online: impl Fn(AccountId) -> bool,
        activity: impl Fn(AccountId) -> Option<Activity>,
    ) -> Live {
        let online = self.online(online);
        let activities = online
            .iter()
            .filter_map(|&account_id| {
                Some(MemberActivity {
                    account_id,
                    activity: activity(account_id)?,
                })
            })
            .collect();
        Live { online, activities }
    }
    /// The full list with live presence applied.
    pub(super) fn members(&self, live: &Live) -> Vec<GuildMember> {
        let mut members: Vec<_> = self.rows.iter().map(|row| row.member.clone()).collect();
        live.apply(&mut members);
        members
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
/// name, with each member's highest-ranked role (name and rank only). Requires
/// current membership; runs on a blocking worker. Role and assignment changes
/// are guild mutations, which refresh member lists after commit.
pub(super) fn snapshot(pool: &DbPool, token: &str, guild: GuildId) -> Result<MemberList, Failure> {
    checked(pool, token, guild, None, None, |c, actor, _| {
        Ok(MemberList {
            rows: query(
                c,
                "SELECT jsonb_build_object('member',jsonb_strip_nulls(jsonb_build_object('account_id',a.id,'username',a.username,'display_name',COALESCE(m.nickname,a.display_name),'avatar_id',v.id,'online',false,'owner',a.id=g.owner,'bot',a.is_bot,'custom_status',CASE WHEN s.account_id IS NULL THEN NULL ELSE jsonb_build_object('emoji',s.emoji,'text',s.status_text,'expires_at',s.expires_at) END,'role',CASE WHEN t.id IS NULL THEN NULL ELSE jsonb_strip_nulls(jsonb_build_object('id',t.id,'name',t.name,'position',t.position,'color',t.color)) END,'hoisted_role',CASE WHEN ho.id IS NULL THEN NULL ELSE jsonb_strip_nulls(jsonb_build_object('id',ho.id,'name',ho.name,'position',ho.position,'color',ho.color)) END,'color',co.color)),'hidden',h.hidden) AS data FROM guild_members m JOIN guilds g ON g.id=m.guild_id JOIN accounts a ON a.id=m.account_id LEFT JOIN account_avatars v ON v.account_id=a.id LEFT JOIN LATERAL (SELECT r.id,r.name,r.position,r.color FROM guild_member_roles mr JOIN guild_roles r ON r.guild_id=mr.guild_id AND r.id=mr.role_id WHERE mr.guild_id=m.guild_id AND mr.account_id=m.account_id AND NOT r.everyone ORDER BY r.position DESC,r.name,r.id LIMIT 1) t ON true LEFT JOIN LATERAL (SELECT r.id,r.name,r.position,r.color FROM guild_member_roles mr JOIN guild_roles r ON r.guild_id=mr.guild_id AND r.id=mr.role_id WHERE mr.guild_id=m.guild_id AND mr.account_id=m.account_id AND r.hoist ORDER BY r.position DESC,r.name,r.id LIMIT 1) ho ON true LEFT JOIN LATERAL (SELECT r.color FROM guild_member_roles mr JOIN guild_roles r ON r.guild_id=mr.guild_id AND r.id=mr.role_id WHERE mr.guild_id=m.guild_id AND mr.account_id=m.account_id AND r.color IS NOT NULL ORDER BY r.position DESC,r.name,r.id LIMIT 1) co ON true CROSS JOIN LATERAL (SELECT EXISTS(SELECT 1 FROM account_blocks b WHERE (b.blocker_id=$2::uuid AND b.blocked_id=a.id) OR (b.blocker_id=a.id AND b.blocked_id=$2::uuid)) AS hidden) h LEFT JOIN account_custom_statuses s ON s.account_id=a.id AND NOT h.hidden AND (s.expires_at IS NULL OR s.expires_at>now()) WHERE m.guild_id=$1::uuid ORDER BY lower(COALESCE(m.nickname,NULLIF(a.display_name,''),a.username)),a.username,a.id LIMIT $3::integer",
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
        "SELECT jsonb_strip_nulls(jsonb_build_object('account_id',a.id,'username',a.username,'display_name',a.display_name,'nickname',m.nickname,'bio',a.bio,'avatar_id',v.id,'owner',a.id=g.owner,'bot',a.is_bot,'custom_status',CASE WHEN s.account_id IS NULL THEN NULL ELSE jsonb_build_object('emoji',s.emoji,'text',s.status_text,'expires_at',s.expires_at) END)) AS data FROM guild_members m JOIN guilds g ON g.id=m.guild_id JOIN accounts a ON a.id=m.account_id LEFT JOIN account_avatars v ON v.account_id=a.id LEFT JOIN account_custom_statuses s ON s.account_id=a.id AND (s.expires_at IS NULL OR s.expires_at>now()) AND NOT EXISTS(SELECT 1 FROM account_blocks b WHERE (b.blocker_id=$3::uuid AND b.blocked_id=a.id) OR (b.blocker_id=a.id AND b.blocked_id=$3::uuid)) WHERE m.guild_id=$1::uuid AND m.account_id=$2::uuid",
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
        // A blocked member never appears online or playing, even with a live socket.
        let live = members.live(
            |_| true,
            |id| (id != a).then(|| Activity::playing("Factorio")),
        );
        assert_eq!(live.online, vec![a, c]);
        assert_eq!(
            live.activities,
            vec![MemberActivity {
                account_id: c,
                activity: Activity::playing("Factorio")
            }]
        );
        let view = members.members(&live);
        assert_eq!(
            view.iter()
                .map(|m| (m.account_id, m.online, m.activity.is_some()))
                .collect::<Vec<_>>(),
            vec![(a, true, false), (b, false, false), (c, true, true)]
        );
        // Offline members have no activity.
        let idle = members.live(|id| id == a, |_| Some(Activity::playing("Factorio")));
        assert_eq!(idle.activities.len(), 1);
        assert_eq!(idle.activities[0].account_id, a);
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
