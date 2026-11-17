//! Platform-independent huddle logic for the voice UI (docs/huddles.md): the
//! plan a roster shows, the optimistic overlay shown until rosters catch up,
//! and which way the split/regroup shortcut goes. The Leptos layer only renders.
use std::collections::BTreeMap;
use thiscord_shared::{
    AccountId,
    huddles::{HuddleAssignment, HuddleState},
    voice::Participant,
};

/// The plan a roster shows.
pub fn state_of(members: &[Participant]) -> HuddleState {
    let mut assignments: Vec<_> = members
        .iter()
        .filter(|m| m.huddle != 0)
        .map(|m| HuddleAssignment {
            account_id: m.account_id,
            huddle: m.huddle,
        })
        .collect();
    assignments.sort_by_key(|a| a.account_id);
    HuddleState {
        split: members.iter().any(|m| m.split),
        split_by: members.iter().find_map(|m| m.split_by),
        assignments,
    }
}

/// A change the server accepted, with the roster state it replaced.
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub before: HuddleState,
    pub after: HuddleState,
}

impl Pending {
    /// Shows `after` only while the roster still shows exactly `before`. Once
    /// rosters catch up, or someone else changes the plan, the roster wins, so
    /// a stale overlay can never decide what the next action does.
    pub fn overlay(&self, members: Vec<Participant>) -> Vec<Participant> {
        if !same_plan(&state_of(&members), &self.before) {
            return members;
        }
        let assigned: BTreeMap<AccountId, u8> = self
            .after
            .assignments
            .iter()
            .map(|a| (a.account_id, a.huddle))
            .collect();
        members
            .into_iter()
            .map(|mut m| {
                m.huddle = assigned.get(&m.account_id).copied().unwrap_or(0);
                m.split = self.after.split;
                m.split_by = self.after.split_by;
                m
            })
            .collect()
    }
}

fn same_plan(a: &HuddleState, b: &HuddleState) -> bool {
    a.split == b.split && a.assignments == b.assignments
}

/// What the split/regroup shortcut asks for: regroup a split room, split a
/// planned one, or nothing (`None`) when nobody is in a huddle yet.
pub fn toggle(state: &HuddleState) -> Option<bool> {
    if state.split {
        Some(false)
    } else if state.assignments.is_empty() {
        None
    } else {
        Some(true)
    }
}

/// Everyone `me` hears in a split room, in roster order, or `None` together.
pub fn companions(members: &[Participant], me: AccountId) -> Option<(u8, Vec<&str>)> {
    let mine = members.iter().find(|m| m.account_id == me && m.split)?;
    Some((
        mine.huddle,
        members
            .iter()
            .filter(|m| m.account_id != me && m.hears(mine))
            .map(Participant::display_name)
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(n: u128, huddle: u8, split: bool) -> Participant {
        let mut m: Participant = serde_json::from_value(serde_json::json!({
            "account_id": format!("00000000-0000-0000-0000-{n:012}"), "username": format!("u{n}"),
            "slot": n, "muted": false, "deafened": false, "can_speak": true
        }))
        .unwrap();
        m.huddle = huddle;
        m.split = split;
        m
    }
    fn id(n: u128) -> AccountId {
        serde_json::from_value(serde_json::json!(format!(
            "00000000-0000-0000-0000-{n:012}"
        )))
        .unwrap()
    }

    #[test]
    fn toggle_direction_follows_the_roster() {
        let together = [member(1, 0, false), member(2, 0, false)];
        assert_eq!(toggle(&state_of(&together)), None);
        let planned = [member(1, 1, false), member(2, 0, false)];
        assert_eq!(toggle(&state_of(&planned)), Some(true));
        let split = [member(1, 1, true), member(2, 0, true)];
        assert_eq!(toggle(&state_of(&split)), Some(false));
    }

    #[test]
    fn overlay_shows_an_accepted_change_until_rosters_catch_up() {
        let planned = vec![member(1, 1, false), member(2, 0, false)];
        let pending = Pending {
            before: state_of(&planned),
            after: HuddleState {
                split: true,
                split_by: Some(id(1)),
                assignments: state_of(&planned).assignments,
            },
        };
        // The roster has not caught up: show the accepted split.
        let shown = pending.overlay(planned.clone());
        assert!(shown.iter().all(|m| m.split && m.split_by == Some(id(1))));
        assert_eq!(toggle(&state_of(&shown)), Some(false));
        // Caught up: the roster is shown as is.
        let caught_up = vec![member(1, 1, true), member(2, 0, true)];
        assert_eq!(pending.overlay(caught_up.clone()), caught_up);
    }

    #[test]
    fn someone_elses_later_change_beats_a_stale_overlay() {
        // I split; before my overlay expires another planner regroups and the
        // roster already shows that. My next shortcut press must split again,
        // not send a second regroup.
        let planned = vec![member(1, 1, false), member(2, 0, false)];
        let pending = Pending {
            before: state_of(&planned),
            after: HuddleState {
                split: true,
                split_by: Some(id(1)),
                assignments: state_of(&planned).assignments,
            },
        };
        let regrouped_by_other = vec![member(1, 2, false), member(2, 0, false)];
        let shown = pending.overlay(regrouped_by_other.clone());
        assert_eq!(shown, regrouped_by_other);
        assert_eq!(toggle(&state_of(&shown)), Some(true));
    }

    #[test]
    fn companions_are_the_people_you_hear() {
        let mut members = vec![member(1, 1, true), member(2, 1, true), member(3, 2, true)];
        members[1].display_name = "Dave".into();
        assert_eq!(companions(&members, id(1)), Some((1, vec!["Dave"])));
        assert_eq!(companions(&members, id(3)), Some((2, vec![])));
        members.iter_mut().for_each(|m| m.split = false);
        assert_eq!(companions(&members, id(1)), None);
    }
}
