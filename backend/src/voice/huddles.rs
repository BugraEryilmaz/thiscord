//! Huddles split a live voice room into private sub-calls without anyone
//! reconnecting (docs/huddles.md). The plan lives with the room: it survives
//! people leaving and rejoining while anyone stays, and ends with the room.
//!
//! Members' `Participant::{huddle, split}` mirror the plan under the room write
//! lock. `viewing::receives`, which both SFU transports run before admission
//! and again on every write, reads only those fields, so a split takes effect
//! for packets already queued as well.
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
use std::sync::{Arc, Weak};
use thiscord_shared::{
    AccountId, ChannelId, GuildId, RequestId,
    huddles::*,
    permissions::{ChannelKind, Permission},
    voice::ROOM_CAPACITY,
};

/// Assignments are kept for people who left while the room lives on. Past this
/// many, the assignments of people no longer in the room are forgotten.
const REMEMBERED: usize = 2 * ROOM_CAPACITY;

#[derive(Debug, Default)]
pub(super) struct Plan {
    split: bool,
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
        let changed = member.info.huddle != huddle || member.info.split != self.split;
        member.info.huddle = huddle;
        member.info.split = self.split;
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
        members: &HashMap<usize, Member>,
    ) -> Result<(), Failure> {
        let present = |account: &AccountId| members.values().any(|m| m.info.account_id == *account);
        match change {
            HuddleChange::Assign { account_id, huddle } => {
                if !present(&account_id) {
                    return Err(Failure::Invalid("That person is not in this voice channel"));
                }
                if huddle == 0 {
                    self.assignments.remove(&account_id);
                } else {
                    self.assignments.insert(account_id, huddle);
                }
                if self.assignments.len() > REMEMBERED {
                    self.assignments.retain(|account, _| present(account));
                }
            }
            HuddleChange::Split { active } => self.split = active,
            HuddleChange::Clear {} => {
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
            assignments,
        }
    }
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
        let (guild, channel) = (request.guild_id, request.channel_id);
        crate::performance::blocking(move || authorize(&pool, &token, guild, channel))
            .await
            .map_err(|_| Failure::Unavailable)??;
        let room = live(guild, channel)
            .await
            .ok_or(Failure::Invalid("Nobody is in this voice channel"))?;
        change(&room, request.change).await
    }
    .await;
    let mut response = match result {
        Ok(state) => {
            tracing::info!(request_id = %id, "voice huddles changed");
            Json(state).into_response()
        }
        Err(error) => error.response(id),
    };
    response
        .headers_mut()
        .insert("cache-control", "no-store".parse().unwrap());
    response
}

async fn live(guild: GuildId, channel: ChannelId) -> Option<Arc<Room>> {
    ROOMS
        .get_or_init(Default::default)
        .lock()
        .await
        .get(&channel)
        .and_then(Weak::upgrade)
        .filter(|room| room.notification_target == Some((guild, channel)))
}

/// Applies an authorized change and returns the resulting plan.
pub(super) async fn change(room: &Room, change: HuddleChange) -> Result<HuddleState, Failure> {
    let mut members = room.members.write().await;
    let mut plan = room.huddles.lock().unwrap();
    plan.change(change, &members)?;
    if plan.apply(&mut members) {
        room.changed.send_replace(());
    }
    Ok(plan.state(&members))
}

/// Current session, guild membership, ViewChannel, JoinVoice and ManageHuddles
/// in that guild voice channel, checked under a shared guild lock. Private
/// calls have no huddles. The plan grants no access, so no revision is needed.
fn authorize(
    pool: &DbPool,
    token: &str,
    guild: GuildId,
    channel: ChannelId,
) -> Result<(), Failure> {
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
        if allowed {
            Ok(())
        } else {
            Err(Failure::Forbidden)
        }
    })
}
