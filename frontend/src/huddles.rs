//! Platform-independent huddle logic for the voice UI (docs/huddles.md): the
//! plan a roster shows, the optimistic overlay shown until rosters catch up,
//! and which way the split/regroup shortcut goes. The Leptos layer only renders.
use std::collections::BTreeMap;
use thiscord_shared::{
    AccountId, ChannelId,
    huddles::{HuddleAssignment, HuddleState},
    voice::{Participant, ROOM_CAPACITY},
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

/// Accepted changes the roster does not show yet: `after` is the server's
/// latest reply and `behind` the roster states that precede it (the state
/// before the first change, then each earlier reply).
#[derive(Debug, Clone, PartialEq)]
pub struct Pending {
    pub behind: Vec<HuddleState>,
    pub after: HuddleState,
}

impl Pending {
    /// Shows `after` only while the roster still shows a state from `behind`.
    /// Once rosters catch up, or someone else changes the plan, the roster wins,
    /// so a stale overlay can never decide what the next action does.
    pub fn overlay(&self, members: Vec<Participant>) -> Vec<Participant> {
        let now = state_of(&members);
        if !self.behind.iter().any(|b| same_plan(&now, b)) {
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

/// What one participant can hear, as far as cues and announcements care.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct View {
    pub channel: ChannelId,
    pub split: bool,
    pub huddle: u8,
}

/// A change worth telling this participant about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// Joined (or switched to) a room that is already split: describe it, no cue.
    Joined,
    Split,
    Regrouped,
    /// Moved to another huddle while the room stays split.
    Moved,
}

/// Compares two views of the same participant. `None` means not in a call;
/// callers keep the last view while reconnecting, so a reconnect is no change.
pub fn transition(previous: Option<&View>, next: Option<&View>) -> Option<Event> {
    let next = next?;
    match previous {
        Some(p) if p.channel == next.channel => match (p.split, next.split) {
            (false, true) => Some(Event::Split),
            (true, false) => Some(Event::Regrouped),
            (true, true) if p.huddle != next.huddle => Some(Event::Moved),
            _ => None,
        },
        _ => next.split.then_some(Event::Joined),
    }
}

/// Whether someone dragged from `from` may be dropped on `target`: another
/// channel with room, among `allowed` (the channels a move could use; empty
/// means unknown, from an older server, and the server still decides).
pub fn can_drop(
    from: ChannelId,
    target: ChannelId,
    occupants: usize,
    allowed: &[ChannelId],
) -> bool {
    from != target && occupants < ROOM_CAPACITY && (allowed.is_empty() || allowed.contains(&target))
}

/// The overlay for a newly accepted change. While earlier accepted changes are
/// still ahead of the roster, keep them as `behind`, so a roster that has
/// caught up with only an earlier change does not bounce people back.
pub fn chain(earlier: Option<&Pending>, roster: &[Participant], after: HuddleState) -> Pending {
    let now = state_of(roster);
    let mut behind = match earlier {
        Some(e)
            if e.behind
                .iter()
                .chain([&e.after])
                .any(|b| same_plan(&now, b)) =>
        {
            let mut behind = e.behind.clone();
            behind.push(e.after.clone());
            behind
        }
        _ => vec![],
    };
    behind.push(now);
    // A burst of changes stays bounded.
    let excess = behind.len().saturating_sub(8);
    behind.drain(..excess);
    Pending { behind, after }
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
            behind: vec![state_of(&planned)],
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
            behind: vec![state_of(&planned)],
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

    fn view(channel: u128, split: bool, huddle: u8) -> View {
        View {
            channel: serde_json::from_value(serde_json::json!(format!(
                "00000000-0000-0000-0000-{channel:012}"
            )))
            .unwrap(),
            split,
            huddle,
        }
    }

    #[test]
    fn only_real_changes_are_announced() {
        use Event::*;
        let t = |a: Option<View>, b: Option<View>| transition(a.as_ref(), b.as_ref());
        // Joining a split room describes it; joining a together room says nothing.
        assert_eq!(t(None, Some(view(1, true, 2))), Some(Joined));
        assert_eq!(t(None, Some(view(1, false, 2))), None);
        // A reconnect keeps the last view, so nothing happened.
        assert_eq!(t(Some(view(1, true, 2)), Some(view(1, true, 2))), None);
        assert_eq!(
            t(Some(view(1, false, 0)), Some(view(1, true, 1))),
            Some(Split)
        );
        assert_eq!(
            t(Some(view(1, true, 1)), Some(view(1, false, 1))),
            Some(Regrouped)
        );
        assert_eq!(
            t(Some(view(1, true, 1)), Some(view(1, true, 4))),
            Some(Moved)
        );
        // Planning while together changes nobody's audio.
        assert_eq!(t(Some(view(1, false, 0)), Some(view(1, false, 3))), None);
        // Switching channels is joining another room.
        assert_eq!(
            t(Some(view(1, true, 1)), Some(view(2, true, 1))),
            Some(Joined)
        );
        assert_eq!(t(Some(view(1, true, 1)), Some(view(2, false, 0))), None);
        assert_eq!(t(Some(view(1, true, 1)), None), None);
    }

    #[test]
    fn drops_need_another_channel_with_room_that_the_move_could_use() {
        let (a, b, c) = (
            view(1, false, 0).channel,
            view(2, false, 0).channel,
            view(3, false, 0).channel,
        );
        assert!(can_drop(a, b, 3, &[a, b]));
        assert!(!can_drop(a, a, 3, &[a, b]));
        assert!(!can_drop(a, b, ROOM_CAPACITY, &[a, b]));
        assert!(!can_drop(a, c, 0, &[a, b]));
        // Older servers do not say; the server still checks on drop.
        assert!(can_drop(a, c, 0, &[]));
    }

    #[test]
    fn chained_changes_do_not_bounce_back() {
        let roster = vec![member(1, 0, false), member(2, 0, false)];
        let first_after = HuddleState {
            assignments: state_of(&[member(1, 1, false)]).assignments,
            ..Default::default()
        };
        let first = chain(None, &roster, first_after);
        let both = vec![member(1, 1, false), member(2, 1, false)];
        let second = chain(Some(&first), &roster, state_of(&both));
        // Neither change in the roster yet: both are shown.
        assert_eq!(state_of(&second.overlay(roster.clone())), state_of(&both));
        // The roster catches up with the first change only: still no bounce.
        let partly = vec![member(1, 1, false), member(2, 0, false)];
        assert_eq!(state_of(&second.overlay(partly)), state_of(&both));
        // Someone else's change wins.
        let other = vec![member(1, 3, false), member(2, 0, false)];
        assert_eq!(second.overlay(other.clone()), other);
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
