//! Huddles split a live voice room into private sub-calls without anyone
//! reconnecting (docs/huddles.md). The plan lives with the room: it survives
//! people leaving and rejoining while anyone stays, and ends with the room.
//!
//! Members' `Participant::{huddle, split, split_by}` mirror the plan under the
//! room write lock. `viewing::receives`, which both SFU transports run before
//! admission and again on every write, reads only those fields, so a split
//! takes effect for packets already queued as well.
use super::{Member, ROOMS, Room};
use crate::{
    auth::{
        Failure,
        store::{self as auth, connection, execute},
    },
    db::DbPool,
    permissions::{authorization::load, evaluator::effective},
};
use axum::{
    Extension, Json, Router,
    extract::{State, rejection::JsonRejection},
    http::HeaderMap,
    response::{IntoResponse, Response},
    routing::post,
};
use diesel::Connection;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant};
use thiscord_shared::{
    AccountId, RequestId,
    huddles::*,
    permissions::{ChannelKind, Permission},
    voice::ROOM_CAPACITY,
};

/// Assignments are kept for people who left while the room lives on. Past this
/// many, the assignments of people no longer in the room are forgotten.
const REMEMBERED: usize = 2 * ROOM_CAPACITY;
/// Changes per account per window: enough to plan a full room twice over and
/// toggle a few times, far below what could load the SFU or the database.
const RATE: (usize, Duration) = (30, Duration::from_secs(10));

#[derive(Debug, Default)]
pub(super) struct Plan {
    split: bool,
    /// Who last changed who hears whom (split, regroup, or moving someone while
    /// split), shown to everyone in the room.
    split_by: Option<AccountId>,
    assignments: BTreeMap<AccountId, u8>,
}

impl Plan {
    /// Mirrors the plan into one member's participant info.
    pub(super) fn apply_one(&self, member: &mut Member) -> bool {
        let huddle = self
            .assignments
            .get(&member.info.account_id)
            .copied()
            .unwrap_or(0);
        let info = &mut member.info;
        let changed =
            info.huddle != huddle || info.split != self.split || info.split_by != self.split_by;
        info.huddle = huddle;
        info.split = self.split;
        info.split_by = self.split_by;
        changed
    }

    /// Mirrors the plan into every member. Call under the room write lock.
    pub(super) fn apply(&self, members: &mut HashMap<usize, Member>) -> bool {
        let mut changed = false;
        for member in members.values_mut() {
            changed |= self.apply_one(member);
        }
        changed
    }

    fn change(
        &mut self,
        change: HuddleChange,
        actor: AccountId,
        members: &HashMap<usize, Member>,
    ) -> Result<(), Failure> {
        let present = |account: &AccountId| members.values().any(|m| m.info.account_id == *account);
        match change {
            HuddleChange::Assign { account_id, huddle } => {
                if !present(&account_id) {
                    return Err(Failure::Invalid("That person is not in this voice channel"));
                }
                let previous = if huddle == 0 {
                    self.assignments.remove(&account_id)
                } else {
                    self.assignments.insert(account_id, huddle)
                };
                // Moving someone while split changes who hears whom.
                if self.split && previous.unwrap_or(0) != huddle {
                    self.split_by = Some(actor);
                }
                if self.assignments.len() > REMEMBERED {
                    self.assignments.retain(|account, _| present(account));
                }
            }
            HuddleChange::Split { active } => {
                if self.split != active {
                    self.split_by = Some(actor);
                }
                self.split = active;
            }
            HuddleChange::Clear {} => {
                if self.split {
                    self.split_by = Some(actor);
                }
                self.split = false;
                self.assignments.clear();
            }
        }
        Ok(())
    }

    fn state(&self, members: &HashMap<usize, Member>) -> HuddleState {
        let mut assignments: Vec<_> = members
            .values()
            .filter(|m| m.info.huddle != 0)
            .map(|m| HuddleAssignment {
                account_id: m.info.account_id,
                huddle: m.info.huddle,
            })
            .collect();
        assignments.sort_by_key(|a| a.account_id);
        HuddleState {
            split: self.split,
            split_by: self.split_by,
            assignments,
        }
    }
}

/// Applies a change under the room write lock and returns the resulting plan.
fn apply(
    room: &Room,
    members: &mut HashMap<usize, Member>,
    change: HuddleChange,
    actor: AccountId,
) -> Result<HuddleState, Failure> {
    let mut plan = room.huddles.lock().unwrap_or_else(|p| p.into_inner());
    plan.change(change, actor, members)?;
    if plan.apply(members) {
        room.changed.send_replace(());
    }
    Ok(plan.state(members))
}

/// Async form for tests and in-process callers that already authorized `actor`.
#[cfg(test)]
pub(super) async fn change(
    room: &Room,
    change: HuddleChange,
    actor: AccountId,
) -> Result<HuddleState, Failure> {
    let mut members = room.members.write().await;
    apply(room, &mut members, change, actor)
}

/// Sliding-window limit per session credential, checked before any database
/// work so refused requests are bounded too (in memory, one backend process).
/// Keys are hashes; the credential itself is not retained.
fn admit(key: u64, now: Instant) -> Result<(), Failure> {
    static RECENT: OnceLock<Mutex<HashMap<u64, Vec<Instant>>>> = OnceLock::new();
    let mut recent = RECENT
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|p| p.into_inner());
    recent.retain(|_, times| {
        times.retain(|t| now.duration_since(*t) < RATE.1);
        !times.is_empty()
    });
    let times = recent.entry(key).or_default();
    if times.len() >= RATE.0 {
        return Err(Failure::Limited);
    }
    times.push(now);
    Ok(())
}

pub(super) fn router() -> Router<Option<DbPool>> {
    Router::new().route(HUDDLES_PATH, post(handle))
}

async fn handle(
    State(pool): State<Option<DbPool>>,
    Extension(id): Extension<RequestId>,
    headers: HeaderMap,
    body: Result<Json<HuddleRequest>, JsonRejection>,
) -> Response {
    let result = async {
        let Json(request) = body.map_err(|_| Failure::Invalid("Invalid huddle request"))?;
        if !request.change.valid() {
            return Err(Failure::Invalid("Huddles are numbered 1 to 4"));
        }
        let pool = pool.ok_or(Failure::Unavailable)?;
        let token = crate::auth::bearer(&headers);
        admit(credential_key(&token), Instant::now())?;
        crate::performance::blocking(move || authorized_change(&pool, &token, &request))
            .await
            .map_err(|_| Failure::Unavailable)?
            .map(|(actor, state)| (actor, request, state))
    }
    .await;
    let mut response = match result {
        Ok((actor, request, state)) => {
            // Account and channel IDs only: attributable without logging names.
            tracing::info!(request_id = %id, actor = %actor, guild = %request.guild_id,
                channel = %request.channel_id, action = action(request.change),
                split = state.split, "voice huddles changed");
            Json(state).into_response()
        }
        Err(error) => error.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

fn credential_key(token: &str) -> u64 {
    use std::hash::BuildHasher;
    static SEED: OnceLock<std::collections::hash_map::RandomState> = OnceLock::new();
    SEED.get_or_init(Default::default).hash_one(token)
}

fn action(change: HuddleChange) -> &'static str {
    match change {
        HuddleChange::Assign { .. } => "assign",
        HuddleChange::Split { active: true } => "split",
        HuddleChange::Split { active: false } => "regroup",
        HuddleChange::Clear {} => "clear",
    }
}

/// Checks the session, guild membership and ViewChannel, JoinVoice and
/// ManageHuddles in that guild voice channel under a shared guild lock, and
/// applies the change before that transaction ends, so a permission change
/// (which locks the guild exclusively) cannot commit between check and apply.
/// Private calls have no huddles. The plan grants no access, so no guild
/// revision is needed. No room lock is ever held while waiting for the guild.
fn authorized_change(
    pool: &DbPool,
    token: &str,
    request: &HuddleRequest,
) -> Result<(AccountId, HuddleState), Failure> {
    let (guild, channel) = (request.guild_id, request.channel_id);
    let mut c = connection(pool)?;
    let session = auth::authenticate(&mut c, token)?;
    session.require_guild(guild)?;
    if guild.as_uuid() == channel.as_uuid() {
        return Err(Failure::Forbidden);
    }
    c.transaction(|c| {
        auth::read_session(c, token, &session)?;
        execute(
            c,
            "SELECT id FROM guilds WHERE id=$1::uuid FOR SHARE",
            &[&guild.to_string()],
        )?;
        let actor = session.account_id;
        let state = load(c, guild, &[actor], Some(channel))?;
        let voice = state
            .channels
            .iter()
            .any(|ch| ch.id == channel && ch.kind == ChannelKind::Voice);
        let permissions = effective(&state, actor, Some(channel));
        let allowed = state.members.iter().any(|m| m.account_id == actor)
            && voice
            && [
                Permission::ViewChannel,
                Permission::JoinVoice,
                Permission::ManageHuddles,
            ]
            .iter()
            .all(|p| permissions.contains(p));
        if !allowed {
            return Err(Failure::Forbidden);
        }
        let room = ROOMS
            .get_or_init(Default::default)
            .blocking_lock()
            .get(&channel)
            .and_then(Weak::upgrade)
            .filter(|room| room.notification_target == Some((guild, channel)))
            .ok_or(Failure::Invalid("Nobody is in this voice channel"))?;
        let room: Arc<Room> = room;
        let mut members = room.members.blocking_write();
        Ok((actor, apply(&room, &mut members, request.change, actor)?))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_rate_limited_per_credential() {
        let (a, b) = (credential_key("token-a"), credential_key("token-b"));
        assert_ne!(a, b);
        assert_eq!(a, credential_key("token-a"));
        let start = Instant::now();
        for _ in 0..RATE.0 {
            admit(a, start).unwrap();
        }
        assert!(matches!(admit(a, start), Err(Failure::Limited)));
        // Other sessions are unaffected, and the window slides.
        admit(b, start).unwrap();
        admit(a, start + RATE.1).unwrap();
    }
}
