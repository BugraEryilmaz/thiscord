//! Private group conversations: membership, metadata and calls. Messages,
//! history, attachments, reads and typing share the direct-message paths in
//! `store`, authorized through [`locked`] instead of a friendship.
//!
//! Every command runs in the caller's transaction after the session check. It
//! locks the group row first (and, for additions, every involved account in UUID
//! order), then evaluates current membership. See docs/friends.md.
use super::store::Effects;
use crate::{
    access::{self, Scope},
    auth::{
        Failure,
        store::{self as auth, execute, query},
    },
};
use diesel::PgConnection;
use serde::Deserialize;
use std::collections::BTreeSet;
use thiscord_shared::{
    AccountId, ClientMessageId,
    friends::*,
    ids::{CallId, ConversationId},
    notifications::{Kind, Target},
};
use uuid::Uuid;

/// A group row locked in the caller's transaction, with the caller verified as
/// a current member after the lock was acquired.
#[derive(Deserialize)]
pub(crate) struct Locked {
    pub id: ConversationId,
    pub revision: i32,
    pub name: String,
}
#[derive(Deserialize, Clone)]
pub(crate) struct Member {
    pub id: AccountId,
    pub username: String,
    pub display_name: String,
    pub owner: bool,
}
impl Member {
    fn named(&self) -> Named {
        Named {
            id: self.id,
            display_name: self.display_name.clone(),
        }
    }
}
/// Locks first and checks membership in a later statement, so a membership
/// change committed while waiting for the lock is always observed.
pub(crate) fn locked(
    c: &mut PgConnection,
    id: ConversationId,
    actor: AccountId,
    write: bool,
) -> Result<Locked, Failure> {
    let sql = if write {
        "SELECT jsonb_build_object('id',id,'revision',revision,'name',name) AS data FROM group_conversations WHERE id=$1::uuid FOR UPDATE"
    } else {
        "SELECT jsonb_build_object('id',id,'revision',revision,'name',name) AS data FROM group_conversations WHERE id=$1::uuid FOR SHARE"
    };
    let group: Locked = query(c, sql, &[&id.to_string()])?
        .pop()
        .ok_or(Failure::Forbidden)?;
    if query::<bool>(
        c,
        "SELECT to_jsonb(TRUE) AS data FROM group_members WHERE group_id=$1::uuid AND account_id=$2::uuid",
        &[&id.to_string(), &actor.to_string()],
    )? != [true]
    {
        return Err(Failure::Forbidden);
    }
    Ok(group)
}
impl Locked {
    /// In join order. Display names fall back to usernames.
    pub fn members(&self, c: &mut PgConnection) -> Result<Vec<Member>, Failure> {
        query(
            c,
            "SELECT jsonb_build_object('id',a.id,'username',a.username,'display_name',COALESCE(NULLIF(a.display_name,''),a.username),'owner',m.owner) AS data FROM group_members m JOIN accounts a ON a.id=m.account_id WHERE m.group_id=$1::uuid ORDER BY m.joined_at,m.account_id",
            &[&self.id.to_string()],
        )
    }
    fn check(&self, revision: i32) -> Result<(), Failure> {
        if self.revision == revision {
            Ok(())
        } else {
            Err(Failure::Conflict)
        }
    }
    fn bump(&self, c: &mut PgConnection) -> Result<(), Failure> {
        execute(
            c,
            "UPDATE group_conversations SET revision=revision+1 WHERE id=$1::uuid",
            &[&self.id.to_string()],
        )?;
        Ok(())
    }
}
fn json<T: serde::Serialize>(value: &T) -> Result<String, Failure> {
    serde_json::to_string(value).map_err(|_| Failure::Unavailable)
}
/// Accounts with a block in either direction with `actor`, among `ids`.
pub(crate) fn blocked_with(
    c: &mut PgConnection,
    actor: AccountId,
    ids: &[AccountId],
) -> Result<BTreeSet<AccountId>, Failure> {
    let blocked: Vec<AccountId> = query(
        c,
        "SELECT to_jsonb(CASE WHEN blocker_id=$1::uuid THEN blocked_id ELSE blocker_id END) AS data FROM account_blocks WHERE (blocker_id=$1::uuid AND blocked_id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb))) OR (blocked_id=$1::uuid AND blocker_id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb)))",
        &[&actor.to_string(), &json(&ids)?],
    )?;
    Ok(blocked.into_iter().collect())
}
/// Share-locks the added accounts in UUID order, then checks that each is an
/// accepted friend of the adder with no block towards anyone in the group.
fn admit(
    c: &mut PgConnection,
    actor: AccountId,
    existing: &[AccountId],
    added: &[AccountId],
) -> Result<(), Failure> {
    let unique: BTreeSet<_> = added.iter().collect();
    if added.is_empty() || unique.len() != added.len() || added.contains(&actor) {
        return Err(Failure::Invalid("Choose friends to add"));
    }
    if added.iter().any(|id| existing.contains(id)) {
        return Err(Failure::Conflict);
    }
    if existing.len() + added.len() > MAX_GROUP_MEMBERS {
        return Err(Failure::Invalid("A group can have at most 8 members"));
    }
    let all: Vec<AccountId> = existing.iter().chain(added).copied().collect();
    // Every pair the friendship and block checks cover includes an added account,
    // and block, unblock and unfriend take NO KEY UPDATE on both accounts, so a
    // SHARE lock on the added accounts alone serializes those checks. Existing
    // members' rows stay unlocked under the group lock: requests that lock their
    // own account before this group (deletion, notification and report commands)
    // cannot deadlock with an addition.
    let locked: Vec<AccountId> = query(
        c,
        "SELECT to_jsonb(id) AS data FROM accounts WHERE id IN (SELECT value::uuid FROM jsonb_array_elements_text($1::jsonb)) ORDER BY id FOR SHARE",
        &[&json(&added)?],
    )?;
    if locked.len() != added.len() {
        return Err(Failure::Invalid("Choose friends to add"));
    }
    let friends: Vec<AccountId> = query(
        c,
        "SELECT to_jsonb(CASE WHEN f.low_id=$1::uuid THEN f.high_id ELSE f.low_id END) AS data FROM friendships f JOIN accounts a ON a.id=CASE WHEN f.low_id=$1::uuid THEN f.high_id ELSE f.low_id END WHERE f.state='accepted' AND $1::uuid IN (f.low_id,f.high_id) AND NOT a.is_bot AND a.email_verified AND a.id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb)) ORDER BY f.id FOR SHARE OF f",
        &[&actor.to_string(), &json(&added)?],
    )?;
    if friends.len() != added.len() {
        return Err(Failure::Invalid("You can only add accepted friends"));
    }
    // Generic on purpose: never reveals which member blocked whom.
    if query::<bool>(
        c,
        "SELECT to_jsonb(EXISTS(SELECT 1 FROM account_blocks WHERE (blocker_id IN (SELECT value::uuid FROM jsonb_array_elements_text($1::jsonb)) AND blocked_id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb))) OR (blocked_id IN (SELECT value::uuid FROM jsonb_array_elements_text($1::jsonb)) AND blocker_id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb))))) AS data",
        &[&json(&added)?, &json(&all)?],
    )? != [false]
    {
        return Err(Failure::Invalid("Someone can't be added to this group"));
    }
    if !query::<AccountId>(c, "SELECT to_jsonb(account_id) AS data FROM group_members WHERE account_id IN (SELECT value::uuid FROM jsonb_array_elements_text($1::jsonb)) GROUP BY account_id HAVING count(*)>=100", &[&json(&added)?])?.is_empty() {
        return Err(Failure::Invalid("Someone is already in too many groups"));
    }
    Ok(())
}
fn display_name(c: &mut PgConnection, id: AccountId) -> Result<String, Failure> {
    query(c, "SELECT to_jsonb(COALESCE(NULLIF(display_name,''),username)) AS data FROM accounts WHERE id=$1::uuid", &[&id.to_string()])?.pop().ok_or(Failure::Forbidden)
}
/// Posts a content-free system notice, ordered like any other message.
fn notice(
    c: &mut PgConnection,
    group: ConversationId,
    actor: AccountId,
    event: SystemEvent,
) -> Result<(), Failure> {
    let system = SystemMessage {
        actor_name: display_name(c, actor)?,
        event,
    };
    let at = super::store::stamp(c, true, group)?;
    execute(
        c,
        "INSERT INTO direct_messages(id,conversation_id,author_id,client_id,content,created_at,system) VALUES($1::uuid,$2::uuid,$3::uuid,$4::uuid,'',$5::timestamptz,$6::jsonb)",
        &[
            &Uuid::new_v4().to_string(),
            &group.to_string(),
            &actor.to_string(),
            &Uuid::new_v4().to_string(),
            &at.to_rfc3339(),
            &json(&system)?,
        ],
    )?;
    Ok(())
}
/// Groups the account belongs to, as its snapshot lists them; `only` selects one.
pub(super) fn list(
    c: &mut PgConnection,
    actor: AccountId,
    only: Option<ConversationId>,
) -> Result<Vec<Group>, Failure> {
    query(
        c,
        "SELECT jsonb_build_object('conversation_id',g.id,'name',g.name,'revision',g.revision,'members',(SELECT COALESCE(jsonb_agg(jsonb_build_object('person',jsonb_build_object('id',a.id,'username',a.username,'display_name',a.display_name,'avatar_id',v.id),'owner',o.owner,'online',false) ORDER BY o.joined_at,o.account_id),'[]'::jsonb) FROM group_members o JOIN accounts a ON a.id=o.account_id LEFT JOIN account_avatars v ON v.account_id=a.id WHERE o.group_id=g.id),'unread_count',(SELECT count(*) FROM (SELECT 1 FROM direct_messages m WHERE m.conversation_id=g.id AND m.created_at>COALESCE(me.read_at,'-infinity'::timestamptz) AND m.author_id<>$1::uuid AND NOT m.deleted AND m.system IS NULL AND NOT EXISTS(SELECT 1 FROM account_blocks b WHERE (b.blocker_id=$1::uuid AND b.blocked_id=m.author_id) OR (b.blocker_id=m.author_id AND b.blocked_id=$1::uuid)) LIMIT 100) unread)) AS data FROM group_members me JOIN group_conversations g ON g.id=me.group_id WHERE me.account_id=$1::uuid AND ($2='' OR g.id=NULLIF($2,'')::uuid) ORDER BY g.last_message_at DESC,g.id",
        &[
            &actor.to_string(),
            &only.map(|id| id.to_string()).unwrap_or_default(),
        ],
    )
}
/// Co-members across a block in either direction: no presence or typing.
pub(super) fn hidden(
    c: &mut PgConnection,
    actor: AccountId,
) -> Result<BTreeSet<AccountId>, Failure> {
    let ids: Vec<AccountId> = query(
        c,
        "SELECT DISTINCT to_jsonb(o.account_id) AS data FROM group_members me JOIN group_members o ON o.group_id=me.group_id AND o.account_id<>me.account_id JOIN account_blocks b ON (b.blocker_id=me.account_id AND b.blocked_id=o.account_id) OR (b.blocker_id=o.account_id AND b.blocked_id=me.account_id) WHERE me.account_id=$1::uuid",
        &[&actor.to_string()],
    )?;
    Ok(ids.into_iter().collect())
}
/// Every account sharing a group with `id`, for profile-change wake-ups.
pub(crate) fn co_members(c: &mut PgConnection, id: AccountId) -> Result<Vec<AccountId>, Failure> {
    query(
        c,
        "SELECT DISTINCT to_jsonb(o.account_id) AS data FROM group_members me JOIN group_members o ON o.group_id=me.group_id AND o.account_id<>me.account_id WHERE me.account_id=$1::uuid",
        &[&id.to_string()],
    )
}
/// The groups of `id`, for history reloads after an account-deletion cascade.
pub(crate) fn of(c: &mut PgConnection, id: AccountId) -> Result<Vec<ConversationId>, Failure> {
    query(
        c,
        "SELECT to_jsonb(group_id) AS data FROM group_members WHERE account_id=$1::uuid",
        &[&id.to_string()],
    )
}
fn view(c: &mut PgConnection, id: ConversationId, actor: AccountId) -> Result<Group, Failure> {
    list(c, actor, Some(id))?.pop().ok_or(Failure::Forbidden)
}
/// Live calls: active, unexpired and with at least one joined lease.
const LIVE: &str = "k.state='active' AND k.expires_at>now() AND EXISTS(SELECT 1 FROM group_call_members p WHERE p.call_id=k.id AND p.state='joined' AND p.until>now())";
#[derive(Deserialize)]
pub(super) struct CallRow {
    pub call: GroupCall,
    /// When this member's view of the call next changes without a write.
    pub expires_at: thiscord_shared::Timestamp,
}
pub(super) fn calls(c: &mut PgConnection, actor: AccountId) -> Result<Vec<CallRow>, Failure> {
    query(
        c,
        &format!(
            "SELECT jsonb_build_object('call',jsonb_build_object('id',k.id,'conversation_id',k.group_id,'started_by',k.started_by,'participants',COALESCE((SELECT jsonb_agg(p.account_id ORDER BY p.account_id) FROM group_call_members p WHERE p.call_id=k.id AND p.state='joined' AND p.until>now()),'[]'::jsonb),'ringing',EXISTS(SELECT 1 FROM group_call_members r WHERE r.call_id=k.id AND r.account_id=$1::uuid AND r.state='ringing' AND r.until>now())),'expires_at',(SELECT min(p.until) FROM group_call_members p WHERE p.call_id=k.id AND p.until>now() AND (p.state='joined' OR (p.state='ringing' AND p.account_id=$1::uuid)))) AS data FROM group_calls k JOIN group_members me ON me.group_id=k.group_id AND me.account_id=$1::uuid WHERE {LIVE} ORDER BY k.id"
        ),
        &[&actor.to_string()],
    )
}
fn call_view(c: &mut PgConnection, id: CallId, actor: AccountId) -> Result<GroupCall, Failure> {
    calls(c, actor)?
        .into_iter()
        .map(|row| row.call)
        .find(|call| call.id == id)
        .ok_or(Failure::Forbidden)
}
fn ring_changed(fx: &mut Effects, ids: impl IntoIterator<Item = AccountId>) {
    for id in ids {
        fx.call_guards
            .push(crate::chat::access::notification_calls(id).pause());
    }
}
/// Ends the group's live call once no joined lease remains, draining its media.
fn end_if_empty(c: &mut PgConnection, group: &Locked, fx: &mut Effects) -> Result<(), Failure> {
    let ended: Vec<CallId> = query(
        c,
        "UPDATE group_calls k SET state='ended' WHERE group_id=$1::uuid AND state='active' AND NOT EXISTS(SELECT 1 FROM group_call_members p WHERE p.call_id=k.id AND p.state='joined' AND p.until>now()) RETURNING to_jsonb(id) AS data",
        &[&group.id.to_string()],
    )?;
    if !ended.is_empty() {
        for id in &ended {
            fx.guards.push(access::pause(Scope::Guild(
                thiscord_shared::GuildId::from_uuid(id.as_uuid()),
            )));
        }
        let members = group.members(c)?;
        ring_changed(fx, members.iter().map(|m| m.id));
    }
    Ok(())
}
/// Removes `who` and everything that depends on their membership. The access
/// drain on their account precedes commit (see `access::pause`).
fn depart(
    c: &mut PgConnection,
    group: &Locked,
    who: AccountId,
    fx: &mut Effects,
) -> Result<(), Failure> {
    let id = group.id.to_string();
    let who_text = who.to_string();
    execute(
        c,
        "DELETE FROM direct_attachments WHERE conversation_id=$1::uuid AND owner_id=$2::uuid AND message_id IS NULL",
        &[&id, &who_text],
    )?;
    execute(
        c,
        "DELETE FROM group_call_members WHERE account_id=$2::uuid AND call_id IN (SELECT id FROM group_calls WHERE group_id=$1::uuid)",
        &[&id, &who_text],
    )?;
    execute(
        c,
        "DELETE FROM group_members WHERE group_id=$1::uuid AND account_id=$2::uuid",
        &[&id, &who_text],
    )?;
    ring_changed(fx, [who]);
    fx.guards.push(access::pause(Scope::Account(who)));
    end_if_empty(c, group, fx)
}
pub(super) fn command(
    c: &mut PgConnection,
    actor: AccountId,
    request: Request,
    fx: &mut Effects,
) -> Result<Response, Failure> {
    match request {
        Request::CreateGroup {
            client_id,
            members,
            name,
        } => create(c, actor, client_id, members, name, fx),
        Request::AddGroupMembers {
            conversation_id,
            revision,
            members,
        } => {
            let group = locked(c, conversation_id, actor, true)?;
            group.check(revision)?;
            let current = group.members(c)?;
            let ids: Vec<AccountId> = current.iter().map(|m| m.id).collect();
            admit(c, actor, &ids, &members)?;
            for id in &members {
                execute(
                    c,
                    "INSERT INTO group_members(group_id,account_id,read_at) VALUES($1::uuid,$2::uuid,clock_timestamp())",
                    &[&group.id.to_string(), &id.to_string()],
                )?;
            }
            let added = group.members(c)?;
            let people = added
                .iter()
                .filter(|m| members.contains(&m.id))
                .map(Member::named)
                .collect();
            notice(c, group.id, actor, SystemEvent::Added { people })?;
            group.bump(c)?;
            fx.changed.extend(added.iter().map(|m| m.id));
            fx.written = Some(group.id);
            Ok(Response::Group {
                group: view(c, group.id, actor)?,
            })
        }
        Request::RemoveGroupMember {
            conversation_id,
            revision,
            account_id,
        } => {
            let group = locked(c, conversation_id, actor, true)?;
            group.check(revision)?;
            let members = group.members(c)?;
            if !members.iter().any(|m| m.id == actor && m.owner) {
                return Err(Failure::Forbidden);
            }
            let target = members
                .iter()
                .find(|m| m.id == account_id && m.id != actor)
                .ok_or(Failure::Conflict)?
                .named();
            notice(c, group.id, actor, SystemEvent::Removed { person: target })?;
            depart(c, &group, account_id, fx)?;
            group.bump(c)?;
            fx.changed.extend(members.iter().map(|m| m.id));
            fx.written = Some(group.id);
            Ok(Response::Group {
                group: view(c, group.id, actor)?,
            })
        }
        Request::TransferGroup {
            conversation_id,
            revision,
            account_id,
        } => {
            let group = locked(c, conversation_id, actor, true)?;
            group.check(revision)?;
            let members = group.members(c)?;
            if !members.iter().any(|m| m.id == actor && m.owner) {
                return Err(Failure::Forbidden);
            }
            let target = members
                .iter()
                .find(|m| m.id == account_id && m.id != actor)
                .ok_or(Failure::Conflict)?
                .named();
            // Two statements: the one-owner index is checked per row.
            execute(
                c,
                "UPDATE group_members SET owner=false WHERE group_id=$1::uuid AND account_id=$2::uuid",
                &[&group.id.to_string(), &actor.to_string()],
            )?;
            execute(
                c,
                "UPDATE group_members SET owner=true WHERE group_id=$1::uuid AND account_id=$2::uuid",
                &[&group.id.to_string(), &account_id.to_string()],
            )?;
            notice(
                c,
                group.id,
                actor,
                SystemEvent::OwnerChanged { person: target },
            )?;
            group.bump(c)?;
            fx.changed.extend(members.iter().map(|m| m.id));
            fx.written = Some(group.id);
            Ok(Response::Group {
                group: view(c, group.id, actor)?,
            })
        }
        Request::RenameGroup {
            conversation_id,
            revision,
            name,
        } => {
            let group = locked(c, conversation_id, actor, true)?;
            let name = name.trim().to_owned();
            if !valid_group_name(&name) {
                return Err(Failure::Invalid(
                    "Group names can have at most 100 characters, without control characters",
                ));
            }
            // Repeating the rename that produced this revision is idempotent.
            if group.name == name
                && (group.revision == revision || group.revision == revision.saturating_add(1))
            {
                return Ok(Response::Group {
                    group: view(c, group.id, actor)?,
                });
            }
            group.check(revision)?;
            execute(
                c,
                "UPDATE group_conversations SET name=$2 WHERE id=$1::uuid",
                &[&group.id.to_string(), &name],
            )?;
            notice(c, group.id, actor, SystemEvent::Renamed { name })?;
            group.bump(c)?;
            let members = group.members(c)?;
            fx.changed.extend(members.iter().map(|m| m.id));
            fx.written = Some(group.id);
            Ok(Response::Group {
                group: view(c, group.id, actor)?,
            })
        }
        Request::LeaveGroup { conversation_id } => {
            let group = locked(c, conversation_id, actor, true)?;
            let members = group.members(c)?;
            let was_owner = members.iter().any(|m| m.id == actor && m.owner);
            if members.len() > 1 {
                notice(c, group.id, actor, SystemEvent::Left {})?;
            }
            depart(c, &group, actor, fx)?;
            // The removal trigger hands ownership to the longest-standing member.
            if was_owner && let Some(next) = group.members(c)?.into_iter().find(|m| m.owner) {
                notice(
                    c,
                    group.id,
                    actor,
                    SystemEvent::OwnerChanged {
                        person: next.named(),
                    },
                )?;
            }
            if members.len() > 1 {
                group.bump(c)?;
            }
            fx.changed.extend(members.iter().map(|m| m.id));
            fx.written = Some(group.id);
            Ok(Response::Done)
        }
        _ => Err(Failure::Invalid("Unsupported group operation")),
    }
}
/// The group an earlier request with this client ID created, if any.
fn retried(
    c: &mut PgConnection,
    actor: AccountId,
    client_id: ClientMessageId,
    hash: &str,
) -> Result<Option<Response>, Failure> {
    #[derive(Deserialize)]
    struct Previous {
        id: ConversationId,
        hash: String,
    }
    let Some(old) = query::<Previous>(c, "SELECT jsonb_build_object('id',id,'hash',request_hash) AS data FROM group_conversations WHERE created_by=$1::uuid AND client_id=$2::uuid", &[&actor.to_string(), &client_id.to_string()])?.pop() else {
        return Ok(None);
    };
    if old.hash != hash {
        return Err(Failure::Conflict);
    }
    locked(c, old.id, actor, false)?;
    Ok(Some(Response::Group {
        group: view(c, old.id, actor)?,
    }))
}
fn create(
    c: &mut PgConnection,
    actor: AccountId,
    client_id: ClientMessageId,
    members: Vec<AccountId>,
    name: String,
    fx: &mut Effects,
) -> Result<Response, Failure> {
    let name = name.trim().to_owned();
    if !valid_group_name(&name) {
        return Err(Failure::Invalid(
            "Group names can have at most 100 characters, without control characters",
        ));
    }
    let sorted: BTreeSet<_> = members.iter().collect();
    let hash = auth::digest(&json(&(&sorted, &name))?);
    if query::<bool>(
        c,
        "SELECT to_jsonb(email_verified) AS data FROM accounts WHERE id=$1::uuid",
        &[&actor.to_string()],
    )? != [true]
    {
        return Err(Failure::Forbidden);
    }
    if let Some(response) = retried(c, actor, client_id, &hash)? {
        return Ok(response);
    }
    admit(c, actor, &[actor], &members)?;
    if !query::<AccountId>(c, "SELECT to_jsonb(account_id) AS data FROM group_members WHERE account_id=$1::uuid GROUP BY account_id HAVING count(*)>=100", &[&actor.to_string()])?.is_empty() {
        return Err(Failure::Invalid("You are already in too many groups"));
    }
    let id = ConversationId::from_uuid(Uuid::new_v4());
    // A concurrent retry of the same client ID that committed first wins.
    if execute(
        c,
        "INSERT INTO group_conversations(id,name,created_by,client_id,request_hash) VALUES($1::uuid,$2,$3::uuid,$4::uuid,$5) ON CONFLICT (created_by,client_id) DO NOTHING",
        &[
            &id.to_string(),
            &name,
            &actor.to_string(),
            &client_id.to_string(),
            &hash,
        ],
    )? == 0
    {
        return retried(c, actor, client_id, &hash)?.ok_or(Failure::Conflict);
    }
    execute(
        c,
        "INSERT INTO group_members(group_id,account_id,owner,read_at) VALUES($1::uuid,$2::uuid,true,clock_timestamp())",
        &[&id.to_string(), &actor.to_string()],
    )?;
    for member in &members {
        execute(
            c,
            "INSERT INTO group_members(group_id,account_id,read_at) VALUES($1::uuid,$2::uuid,clock_timestamp())",
            &[&id.to_string(), &member.to_string()],
        )?;
    }
    notice(c, id, actor, SystemEvent::Created {})?;
    fx.changed.extend(members.iter().copied());
    fx.written = Some(id);
    Ok(Response::Group {
        group: view(c, id, actor)?,
    })
}
/// Joins the live call or starts one that rings every member not separated
/// from the caller by a block.
pub(super) fn start_call(
    c: &mut PgConnection,
    actor: AccountId,
    group: &Locked,
    fx: &mut Effects,
) -> Result<Response, Failure> {
    let live: Option<CallId> = query(
        c,
        &format!(
            "SELECT to_jsonb(k.id) AS data FROM group_calls k WHERE k.group_id=$1::uuid AND {LIVE}"
        ),
        &[&group.id.to_string()],
    )?
    .pop();
    if let Some(id) = live {
        return join(c, actor, group, id, fx);
    }
    // A lapsed call frees the one-live-call slot.
    execute(
        c,
        "UPDATE group_calls SET state='ended' WHERE group_id=$1::uuid AND state='active'",
        &[&group.id.to_string()],
    )?;
    let id = CallId::from_uuid(Uuid::new_v4());
    execute(
        c,
        "INSERT INTO group_calls(id,group_id,started_by,state) VALUES($1::uuid,$2::uuid,$3::uuid,'active')",
        &[&id.to_string(), &group.id.to_string(), &actor.to_string()],
    )?;
    execute(
        c,
        "INSERT INTO group_call_members(call_id,account_id,state,until) VALUES($1::uuid,$2::uuid,'joined',now()+interval '60 seconds')",
        &[&id.to_string(), &actor.to_string()],
    )?;
    let members = group.members(c)?;
    let others: Vec<AccountId> = members
        .iter()
        .map(|m| m.id)
        .filter(|id| *id != actor)
        .collect();
    let blocked = blocked_with(c, actor, &others)?;
    for other in others.iter().filter(|id| !blocked.contains(id)) {
        execute(
            c,
            "INSERT INTO group_call_members(call_id,account_id,state,until) VALUES($1::uuid,$2::uuid,'ringing',now()+interval '45 seconds')",
            &[&id.to_string(), &other.to_string()],
        )?;
    }
    ring_changed(fx, members.iter().map(|m| m.id));
    fx.changed.extend(members.iter().map(|m| m.id));
    Ok(Response::GroupCall {
        call: call_view(c, id, actor)?,
    })
}
fn join(
    c: &mut PgConnection,
    actor: AccountId,
    group: &Locked,
    id: CallId,
    fx: &mut Effects,
) -> Result<Response, Failure> {
    execute(
        c,
        "INSERT INTO group_call_members(call_id,account_id,state,until) VALUES($1::uuid,$2::uuid,'joined',now()+interval '60 seconds') ON CONFLICT(call_id,account_id) DO UPDATE SET state='joined',until=EXCLUDED.until",
        &[&id.to_string(), &actor.to_string()],
    )?;
    execute(
        c,
        "UPDATE group_calls SET expires_at=GREATEST(expires_at,now()+interval '60 seconds') WHERE id=$1::uuid",
        &[&id.to_string()],
    )?;
    ring_changed(fx, [actor]);
    let members = group.members(c)?;
    fx.changed.extend(members.iter().map(|m| m.id));
    Ok(Response::GroupCall {
        call: call_view(c, id, actor)?,
    })
}
/// The group of a call, if it is a group call (without authorizing).
fn call_group(c: &mut PgConnection, id: CallId) -> Result<Option<ConversationId>, Failure> {
    Ok(query(
        c,
        "SELECT to_jsonb(group_id) AS data FROM group_calls WHERE id=$1::uuid",
        &[&id.to_string()],
    )?
    .pop())
}
pub(super) fn accept_call(
    c: &mut PgConnection,
    actor: AccountId,
    id: CallId,
    fx: &mut Effects,
) -> Result<Response, Failure> {
    let group = call_group(c, id)?.ok_or(Failure::Forbidden)?;
    let group = locked(c, group, actor, true)?;
    if query::<bool>(
        c,
        &format!("SELECT to_jsonb(TRUE) AS data FROM group_calls k WHERE k.id=$1::uuid AND {LIVE}"),
        &[&id.to_string()],
    )? != [true]
    {
        return Err(Failure::Forbidden);
    }
    join(c, actor, &group, id, fx)
}
/// Declines or leaves; the call continues for everyone else still joined.
pub(super) fn end_call(
    c: &mut PgConnection,
    actor: AccountId,
    id: CallId,
    fx: &mut Effects,
) -> Result<Response, Failure> {
    let group = call_group(c, id)?.ok_or(Failure::Forbidden)?;
    let group = locked(c, group, actor, true)?;
    execute(
        c,
        "UPDATE group_call_members SET state=CASE WHEN state='ringing' THEN 'declined' ELSE 'left' END,until=now() WHERE call_id=$1::uuid AND account_id=$2::uuid AND state IN ('ringing','joined')",
        &[&id.to_string(), &actor.to_string()],
    )?;
    fx.guards.push(access::pause(Scope::Member(
        thiscord_shared::GuildId::from_uuid(id.as_uuid()),
        actor,
    )));
    ring_changed(fx, [actor]);
    end_if_empty(c, &group, fx)?;
    let members = group.members(c)?;
    fx.changed.extend(members.iter().map(|m| m.id));
    Ok(Response::Done)
}
/// SFU authorization for a group call: current membership and a joined lease,
/// which this renews. `None` when `id` is not a group call.
pub(super) fn voice(
    c: &mut PgConnection,
    actor: AccountId,
    id: CallId,
) -> Result<Option<()>, Failure> {
    let Some(group) = call_group(c, id)? else {
        return Ok(None);
    };
    locked(c, group, actor, false)?;
    if execute(
        c,
        &format!(
            "UPDATE group_call_members p SET until=now()+interval '60 seconds' FROM group_calls k WHERE p.call_id=$1::uuid AND p.account_id=$2::uuid AND p.state='joined' AND p.until>now() AND k.id=p.call_id AND {LIVE}"
        ),
        &[&id.to_string(), &actor.to_string()],
    )? != 1
    {
        return Err(Failure::Forbidden);
    }
    execute(
        c,
        "UPDATE group_calls SET expires_at=GREATEST(expires_at,now()+interval '60 seconds') WHERE id=$1::uuid",
        &[&id.to_string()],
    )?;
    Ok(Some(()))
}
/// Notification recipients for a group message: everyone else not separated
/// from the author by a block, plus `@username` mentions among them.
pub(super) fn message_candidate(
    c: &mut PgConnection,
    actor: AccountId,
    group: &Locked,
    content: &str,
) -> Result<crate::notifications::Candidate, Failure> {
    let members = group.members(c)?;
    let others: Vec<AccountId> = members
        .iter()
        .map(|m| m.id)
        .filter(|id| *id != actor)
        .collect();
    let blocked = blocked_with(c, actor, &others)?;
    let recipients: Vec<AccountId> = others
        .into_iter()
        .filter(|id| !blocked.contains(id))
        .collect();
    let mentioned = mentions(content);
    let mut event = crate::notifications::Candidate::new(
        Target::Conversation {
            conversation_id: group.id,
        },
        Kind::DirectMessage,
        "New group message".into(),
        Some(actor),
    );
    event.mentions = members
        .iter()
        .filter(|m| {
            recipients.contains(&m.id) && mentioned.contains(&m.username.to_ascii_lowercase())
        })
        .map(|m| m.id)
        .collect();
    event.recipients = Some(recipients);
    event.group = true;
    Ok(event)
}
/// Lowercased `@username` tokens, like guild mentions.
fn mentions(content: &str) -> BTreeSet<String> {
    content
        .split_whitespace()
        .filter_map(|word| word.strip_prefix('@'))
        .map(|name| {
            name.chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|name| !name.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    fn mentions_match_guild_tokenization() {
        let found = super::mentions("hi @Ada, @bob_2! email@x.com @ @Ada");
        assert_eq!(
            found.into_iter().collect::<Vec<_>>(),
            vec!["ada".to_string(), "bob_2".to_string()]
        );
    }
}
