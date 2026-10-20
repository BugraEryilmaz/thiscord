//! Ordering for server navigation. A selection is intent, never an access grant.
use thiscord_shared::GuildId;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Selection {
    generation: u64,
    pub guild: Option<GuildId>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Navigation {
    current: Selection,
    pub pending: bool,
}

impl Navigation {
    pub fn current(&self) -> Selection {
        self.current
    }

    /// Home and session changes also advance the generation, including A → Home → A.
    pub fn begin(&mut self, guild: Option<GuildId>) -> Selection {
        self.current = Selection {
            generation: self.current.generation.wrapping_add(1),
            guild,
        };
        self.pending = guild.is_some();
        self.current
    }

    pub fn accepts(&self, selection: Selection) -> bool {
        self.current == selection
    }

    /// A late success, failure or timeout must not finish a newer request.
    pub fn finish(&mut self, selection: Selection) -> bool {
        if !self.accepts(selection) {
            return false;
        }
        self.pending = false;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn guild(value: u128) -> GuildId {
        format!("{value:032x}").parse().unwrap()
    }

    #[test]
    fn latest_selection_wins_in_both_completion_orders() {
        for newest_first in [false, true] {
            let mut navigation = Navigation::default();
            let old = navigation.begin(Some(guild(1)));
            let new = navigation.begin(Some(guild(2)));
            assert_eq!(navigation.current().guild, Some(guild(2)));
            assert!(navigation.pending);
            if newest_first {
                assert!(navigation.finish(new));
                assert!(!navigation.finish(old));
            } else {
                assert!(!navigation.finish(old));
                assert!(navigation.pending);
                assert!(navigation.finish(new));
            }
            assert!(!navigation.pending);
            assert!(navigation.accepts(new));
        }
    }

    #[test]
    fn revisiting_same_guild_does_not_accept_old_refresh_or_selection() {
        let mut navigation = Navigation::default();
        let old = navigation.begin(Some(guild(1)));
        assert!(navigation.finish(old));
        let refresh = navigation.current();
        navigation.begin(Some(guild(2)));
        let new = navigation.begin(Some(guild(1)));
        assert!(!navigation.accepts(refresh));
        assert!(!navigation.finish(old));
        assert!(navigation.finish(new));
    }

    #[test]
    fn home_and_session_reset_invalidate_in_flight_work() {
        let mut navigation = Navigation::default();
        let old = navigation.begin(Some(guild(1)));
        navigation.begin(None);
        assert!(!navigation.pending);
        assert!(!navigation.finish(old));
        let home_refresh = navigation.current();
        navigation.begin(None); // A new session while already on Home.
        assert!(!navigation.accepts(home_refresh));
        navigation.begin(Some(guild(1)));
        assert!(!navigation.accepts(old));
    }

    #[test]
    fn retry_after_timeout_has_a_new_generation() {
        let mut navigation = Navigation::default();
        let timed_out = navigation.begin(Some(guild(1)));
        assert!(navigation.finish(timed_out));
        let retry = navigation.begin(Some(guild(1)));
        assert!(!navigation.finish(timed_out));
        assert!(navigation.pending);
        assert!(navigation.finish(retry));
    }
}
