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

    /// Ordinary rail clicks reuse the pending selection or already open view.
    /// Explicit refresh/settings actions still begin a fresh authorization read.
    pub fn needs_navigation(&self, guild: GuildId, loaded: Option<GuildId>) -> bool {
        loaded != Some(guild) && !(self.pending && self.current.guild == Some(guild))
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

    #[test]
    fn repeated_pending_click_keeps_original_request_and_allows_retry_after_failure() {
        let mut navigation = Navigation::default();
        let target = guild(1);
        assert!(navigation.needs_navigation(target, None));
        let first = navigation.begin(Some(target));
        // Repeated clicks must not supersede a useful in-flight response.
        for _ in 0..3 {
            assert!(!navigation.needs_navigation(target, None));
            assert!(navigation.accepts(first));
        }
        assert!(navigation.finish(first));
        // No view was loaded (error/timeout), so the same rail button can retry.
        assert!(navigation.needs_navigation(target, None));
        let retry = navigation.begin(Some(target));
        assert!(!navigation.accepts(first));
        assert!(navigation.finish(retry));
        assert!(!navigation.needs_navigation(target, Some(target)));
        // Settings/refresh can still force reauthorization of the open guild.
        let refresh = navigation.begin(Some(target));
        assert!(!navigation.accepts(retry));
        assert!(navigation.finish(refresh));
    }

    #[test]
    fn pending_click_deduplication_does_not_block_switching_or_revisiting() {
        let mut navigation = Navigation::default();
        let first = navigation.begin(Some(guild(1)));
        assert!(navigation.needs_navigation(guild(2), None));
        navigation.begin(Some(guild(2)));
        assert!(navigation.needs_navigation(guild(1), None));
        let revisit = navigation.begin(Some(guild(1)));
        assert!(!navigation.finish(first));
        assert!(!navigation.needs_navigation(guild(1), None));
        assert!(navigation.finish(revisit));
        navigation.begin(None);
        assert!(navigation.needs_navigation(guild(1), None));
    }
}
