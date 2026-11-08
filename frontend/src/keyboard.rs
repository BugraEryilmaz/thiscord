//! Platform-independent keyboard navigation: the global navigation shortcuts,
//! the quick switcher's matching/ranking, and index arithmetic for roving
//! focus in lists and grids. The Leptos layer only reads DOM events and focus.
use std::cmp::Reverse;
use thiscord_shared::{
    ChannelId,
    permissions::{Category, Channel},
};

/// The modifier and key state of a keyboard event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Keys<'a> {
    /// `KeyboardEvent.key`: the produced character or named key.
    pub key: &'a str,
    /// `KeyboardEvent.code`: the physical key.
    pub code: &'a str,
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

/// App-wide commands with recordable shortcuts (Settings → Shortcuts). Each
/// binding has Control, Alt, Super or is a function key, so all of them are
/// safe while typing in the composer (like Discord).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    QuickSwitcher,
    ShortcutHelp,
    /// Handled by the conversation search, not the navigation host.
    Search,
    PreviousChannel,
    NextChannel,
    PreviousUnread,
    NextUnread,
    PreviousServer,
    NextServer,
}

impl Command {
    pub const ALL: [Self; 9] = [
        Self::QuickSwitcher,
        Self::ShortcutHelp,
        Self::Search,
        Self::PreviousChannel,
        Self::NextChannel,
        Self::PreviousUnread,
        Self::NextUnread,
        Self::PreviousServer,
        Self::NextServer,
    ];

    /// English UI catalog key.
    pub fn label(self) -> &'static str {
        match self {
            Self::QuickSwitcher => "Open the quick switcher",
            Self::ShortcutHelp => "Show keyboard shortcuts",
            Self::Search => "Search this conversation",
            Self::PreviousChannel => "Previous text channel",
            Self::NextChannel => "Next text channel",
            Self::PreviousUnread => "Previous unread channel",
            Self::NextUnread => "Next unread channel",
            Self::PreviousServer => "Previous server",
            Self::NextServer => "Next server",
        }
    }

    /// The command for a key press with the default bindings.
    pub fn from_keys(keys: &Keys, typing: bool) -> Option<Self> {
        AppShortcuts::default().command(keys, typing)
    }
}

/// Device-local storage key for [`AppShortcuts`]. Contains no account data.
pub const APP_SHORTCUTS_KEY: &str = "thiscord.app-shortcuts.v1";

/// Why a recorded key cannot be an app shortcut.
pub const COMMAND_KEY: &str = "App shortcuts need Ctrl, Alt or Super, or a function key, so typing and list keys keep working.";

/// Recordable app shortcuts in the binding syntax of `web_shortcuts`. Empty
/// disables a command. Matched by physical key (`KeyboardEvent.code`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct AppShortcuts {
    pub quick_switcher: String,
    pub shortcut_help: String,
    pub search: String,
    pub previous_channel: String,
    pub next_channel: String,
    pub previous_unread: String,
    pub next_unread: String,
    pub previous_server: String,
    pub next_server: String,
}

impl Default for AppShortcuts {
    fn default() -> Self {
        Self {
            quick_switcher: "CmdOrCtrl+K".into(),
            shortcut_help: "CmdOrCtrl+Slash".into(),
            search: "CmdOrCtrl+F".into(),
            previous_channel: "Alt+ArrowUp".into(),
            next_channel: "Alt+ArrowDown".into(),
            previous_unread: "Alt+Shift+ArrowUp".into(),
            next_unread: "Alt+Shift+ArrowDown".into(),
            previous_server: "Control+Alt+ArrowUp".into(),
            next_server: "Control+Alt+ArrowDown".into(),
        }
    }
}

impl AppShortcuts {
    pub fn get(&self, command: Command) -> &str {
        match command {
            Command::QuickSwitcher => &self.quick_switcher,
            Command::ShortcutHelp => &self.shortcut_help,
            Command::Search => &self.search,
            Command::PreviousChannel => &self.previous_channel,
            Command::NextChannel => &self.next_channel,
            Command::PreviousUnread => &self.previous_unread,
            Command::NextUnread => &self.next_unread,
            Command::PreviousServer => &self.previous_server,
            Command::NextServer => &self.next_server,
        }
    }

    pub fn get_mut(&mut self, command: Command) -> &mut String {
        match command {
            Command::QuickSwitcher => &mut self.quick_switcher,
            Command::ShortcutHelp => &mut self.shortcut_help,
            Command::Search => &mut self.search,
            Command::PreviousChannel => &mut self.previous_channel,
            Command::NextChannel => &mut self.next_channel,
            Command::PreviousUnread => &mut self.previous_unread,
            Command::NextUnread => &mut self.next_unread,
            Command::PreviousServer => &mut self.previous_server,
            Command::NextServer => &mut self.next_server,
        }
    }

    /// Valid syntax, a command-safe chord ([`COMMAND_KEY`]) and no duplicates,
    /// also against `others` (the voice and overlay shortcuts on this device).
    pub fn validate(&self, others: &[&str]) -> Result<(), &'static str> {
        use crate::web_shortcuts::{Chord, conflicts};
        let mut seen: Vec<&str> = others
            .iter()
            .copied()
            .filter(|o| !o.trim().is_empty())
            .collect();
        for command in Command::ALL {
            let text = self.get(command).trim();
            if text.is_empty() {
                continue;
            }
            if text.len() > 128 {
                return Err("Shortcut is too long");
            }
            let chord = Chord::parse(text)
                .ok_or("Invalid shortcut. Use a combination such as Control+Shift+M.")?;
            if !chord.while_typing() {
                return Err(COMMAND_KEY);
            }
            if seen.iter().any(|other| conflicts(other, text)) {
                return Err("Each action needs a different shortcut");
            }
            seen.push(text);
        }
        Ok(())
    }

    /// The command for a key press. A binding matches its exact physical
    /// chord. Default bindings keep their compatibility aliases: Ctrl and
    /// Command both act as the primary modifier, "/" from any key opens help
    /// (Turkish Q types it with Shift), Ctrl+F matches by character, and
    /// Ctrl+Shift+↑/↓ switches servers where the OS reserves Ctrl+Alt+Arrow,
    /// except while `typing` (text fields select by paragraph with it).
    pub fn command(&self, keys: &Keys, typing: bool) -> Option<Command> {
        use crate::web_shortcuts::{Chord, Pressed};
        let pressed = Pressed {
            code: keys.code,
            control: keys.control,
            shift: keys.shift,
            alt: keys.alt,
            meta: keys.meta,
        };
        if let Some(command) = Command::ALL.into_iter().find(|&command| {
            Chord::parse(self.get(command)).is_some_and(|chord| chord.matches(&pressed))
        }) {
            return Some(command);
        }
        let defaults = Self::default();
        let default = |command| self.get(command) == defaults.get(command);
        let primary = keys.control != keys.meta;
        if default(Command::ShortcutHelp)
            && primary
            && !keys.alt
            && (keys.code == "Slash" || keys.key == "/")
        {
            return Some(Command::ShortcutHelp);
        }
        if default(Command::Search)
            && (keys.control || keys.meta)
            && !keys.alt
            && keys.key.eq_ignore_ascii_case("f")
        {
            return Some(Command::Search);
        }
        let forward = match keys.key {
            "ArrowUp" => false,
            "ArrowDown" => true,
            _ => return None,
        };
        let server = if forward {
            Command::NextServer
        } else {
            Command::PreviousServer
        };
        (default(server)
            && !typing
            && (keys.control, keys.meta, keys.alt, keys.shift) == (true, false, false, true))
            .then_some(server)
    }
}

/// A binding in display form: "Ctrl+K" or, on Apple platforms, "⌘K" (Command
/// for CmdOrCtrl and Super, ⌃ for an explicit Control).
pub fn display_binding(text: &str, apple: bool) -> String {
    let labels =
        crate::web_shortcuts::labels(text)
            .into_iter()
            .map(|label| match (label.as_str(), apple) {
                ("Ctrl/⌘", false) => "Ctrl".to_owned(),
                ("Ctrl/⌘" | "Super", true) => "⌘".to_owned(),
                ("Ctrl", true) => "⌃".to_owned(),
                ("Alt", true) => "⌥".to_owned(),
                ("Shift", true) => "⇧".to_owned(),
                _ => label,
            });
    labels
        .collect::<Vec<_>>()
        .join(if apple { "" } else { "+" })
}

/// One row in the keyboard shortcut reference: fixed key chords (already in
/// display form, joined with " / " for alternatives) or recordable `commands`
/// shown with their current bindings, and an untranslated description.
pub struct Shortcut {
    pub keys: &'static str,
    pub action: &'static str,
    pub commands: &'static [Command],
}

/// Groups of shortcuts for the help dialog and settings. Descriptions are
/// catalog keys for `i18n::t`.
pub const SHORTCUTS: &[(&str, &[Shortcut])] = &[
    (
        "Navigation",
        &[
            Shortcut {
                keys: "",
                action: "Open the quick switcher",
                commands: &[Command::QuickSwitcher],
            },
            Shortcut {
                keys: "",
                action: "Previous or next text channel",
                commands: &[Command::PreviousChannel, Command::NextChannel],
            },
            Shortcut {
                keys: "",
                action: "Previous or next unread channel",
                commands: &[Command::PreviousUnread, Command::NextUnread],
            },
            Shortcut {
                keys: "",
                action: "Previous or next server",
                commands: &[Command::PreviousServer, Command::NextServer],
            },
            Shortcut {
                keys: "",
                action: "Show keyboard shortcuts",
                commands: &[Command::ShortcutHelp],
            },
        ],
    ),
    (
        "Chat",
        &[
            Shortcut {
                keys: "Esc",
                action: "Jump to latest and mark the channel read; closes menus and dialogs first",
                commands: &[],
            },
            Shortcut {
                keys: "",
                action: "Search this conversation",
                commands: &[Command::Search],
            },
            Shortcut {
                keys: "↑",
                action: "Edit your last message (empty composer)",
                commands: &[],
            },
            Shortcut {
                keys: "Enter / Shift+Enter",
                action: "Send message / new line",
                commands: &[],
            },
        ],
    ),
    (
        "Lists and menus",
        &[
            Shortcut {
                keys: "↑ ↓ Home End",
                action: "Move between servers, channels, members, friends and menu items",
                commands: &[],
            },
            Shortcut {
                keys: "← → ↑ ↓",
                action: "Move in emoji and reaction grids",
                commands: &[],
            },
            Shortcut {
                keys: "Shift+F10 / Menu",
                action: "Open the context menu for the focused item",
                commands: &[],
            },
            Shortcut {
                keys: "Enter / Space",
                action: "Activate the focused item",
                commands: &[],
            },
        ],
    ),
    (
        "Voice",
        &[Shortcut {
            keys: "Ctrl+Shift+Space",
            action: "Push to talk (hold; default key, change it in Settings)",
            commands: &[],
        }],
    ),
];

/// Displays the primary modifier as macOS users expect.
pub fn display_keys(keys: &str, apple: bool) -> String {
    if apple {
        keys.replace("Ctrl+", "⌘").replace("Alt+", "⌥")
    } else {
        keys.to_owned()
    }
}

/// What a quick-switcher entry opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Server,
    TextChannel,
    VoiceChannel,
    Conversation,
}

/// A searchable destination. `context` (server name for channels) is matched
/// with lower weight than the label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    pub kind: Kind,
    pub label: String,
    pub context: String,
    pub unread: bool,
    /// The current server or conversation; listed after others for empty queries.
    pub current: bool,
}

fn fold(text: &str) -> Vec<char> {
    // Turkish İ lowercases to i + U+0307; drop the combining dot so "i" matches.
    text.chars()
        .flat_map(char::to_lowercase)
        .filter(|c| *c != '\u{307}')
        .map(|c| match c {
            'ı' => 'i',
            other => other,
        })
        .collect()
}

/// Match quality of `query` in `text`; higher is better, `None` when not all
/// query characters appear in order. Exact, prefix, word-start and substring
/// matches beat scattered (fuzzy) ones; fuzzy ones prefer fewer gaps.
pub fn score(query: &str, text: &str) -> Option<u32> {
    let query: Vec<char> = fold(query)
        .into_iter()
        .filter(|c| !c.is_whitespace())
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let text = fold(text);
    let compact: Vec<char> = text
        .iter()
        .copied()
        .filter(|c| !c.is_whitespace())
        .collect();
    if compact == query {
        return Some(1000);
    }
    let boundary = |i: usize| i == 0 || !text[i - 1].is_alphanumeric();
    let starts = |i: usize| text[i..].starts_with(&query);
    if starts(0) {
        return Some(900 - (text.len() - query.len()).min(99) as u32);
    }
    if let Some(i) = (1..text.len()).find(|&i| boundary(i) && starts(i)) {
        return Some(800 - i.min(99) as u32);
    }
    if let Some(i) = (1..text.len()).find(|&i| starts(i)) {
        return Some(700 - i.min(99) as u32);
    }
    // Subsequence: reward matches at word starts, penalise gaps.
    let (mut at, mut gaps, mut word_starts) = (0, 0_u32, 0_u32);
    let mut previous: Option<usize> = None;
    for wanted in &query {
        let found = (at..text.len()).find(|&i| text[i] == *wanted)?;
        if previous.is_some_and(|p| found > p + 1) {
            gaps += 1;
        }
        if boundary(found) {
            word_starts += 1;
        }
        previous = Some(found);
        at = found + 1;
    }
    Some(
        (400 + word_starts * 20)
            .saturating_sub(gaps * 15)
            .clamp(1, 600),
    )
}

/// Indices of `candidates` matching `query`, best first, at most `limit`.
/// Equal scores keep other locations before the current one, unread before
/// read, then the given (sidebar) order. An empty query therefore lists unread
/// entries first and the current location last.
pub fn rank(query: &str, candidates: &[Candidate], limit: usize) -> Vec<usize> {
    let mut scored: Vec<(u32, usize)> = candidates
        .iter()
        .enumerate()
        .filter_map(|(index, candidate)| {
            if query.trim().is_empty() {
                return Some((0, index));
            }
            let label = score(query, &candidate.label);
            // Context helps "general dev" style queries without outranking labels.
            let context =
                score(query, &format!("{} {}", candidate.label, candidate.context)).map(|s| s / 2);
            label.max(context).map(|s| (s, index))
        })
        .collect();
    scored.sort_by_key(|&(score, index)| {
        let candidate = &candidates[index];
        (Reverse(score), candidate.current, !candidate.unread, index)
    });
    scored.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// Sidebar channel order: categories by position, pinned channels first within
/// each category, then position and ID. Shared by the list and Alt+↑/↓.
pub fn channel_order<'a>(
    channels: &'a [Channel],
    categories: &[Category],
    pinned: &std::collections::BTreeSet<ChannelId>,
) -> Vec<&'a Channel> {
    let mut ordered: Vec<&Channel> = channels.iter().collect();
    ordered.sort_by_key(|c| {
        (
            c.category_id.and_then(|id| {
                categories
                    .iter()
                    .find(|cat| cat.id == id)
                    .map(|cat| (cat.position, cat.id))
            }),
            !pinned.contains(&c.id),
            c.position,
            c.id,
        )
    });
    ordered
}

/// The next (or previous) index after `current` in a list of `len` entries
/// for which `eligible` holds, wrapping around. Without a current entry it
/// starts from the first (or last) eligible one. Never returns `current`.
pub fn cycle(
    len: usize,
    current: Option<usize>,
    forward: bool,
    eligible: impl Fn(usize) -> bool,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = current.filter(|&c| c < len);
    let start = match (current, forward) {
        (Some(c), true) => c + 1,
        (Some(c), false) => c + len - 1,
        (None, true) => 0,
        (None, false) => len - 1,
    };
    let steps = if current.is_some() { len - 1 } else { len };
    (0..steps)
        .map(|offset| {
            if forward {
                (start + offset) % len
            } else {
                (start + len - offset % len) % len
            }
        })
        .find(|&i| eligible(i))
}

/// Roving focus in a vertical list: ArrowUp/ArrowDown wrap, Home/End jump.
/// `None` for keys the list does not handle.
pub fn list_step(len: usize, current: Option<usize>, key: &str) -> Option<usize> {
    if len == 0 {
        return None;
    }
    match key {
        "ArrowDown" => cycle(len, current, true, |_| true).or(current),
        "ArrowUp" => cycle(len, current, false, |_| true).or(current),
        "Home" => Some(0),
        "End" => Some(len - 1),
        _ => None,
    }
}

/// Arrow-key movement in a wrapped grid, from each item's on-screen center
/// `(x, y)` in reading order. Left/Right follow reading order; Up/Down pick
/// the nearest row above/below and the closest column in it, staying put at
/// the edges. Works for sections of different lengths (emoji groups).
pub fn spatial_step(centers: &[(f64, f64)], current: usize, key: &str) -> Option<usize> {
    let last = centers.len().checked_sub(1)?;
    let current = current.min(last);
    let (x, y) = centers[current];
    let vertical = |down: bool| {
        let row = centers
            .iter()
            .map(|&(_, cy)| cy - y)
            .filter(|dy| if down { *dy > 1.0 } else { *dy < -1.0 })
            .map(f64::abs)
            .fold(f64::INFINITY, f64::min);
        if !row.is_finite() {
            return Some(current);
        }
        centers
            .iter()
            .enumerate()
            .filter(|(_, (_, cy))| ((cy - y).abs() - row).abs() <= 1.0 && (*cy > y) == down)
            .min_by(|(_, (a, _)), (_, (b, _))| (a - x).abs().total_cmp(&(b - x).abs()))
            .map(|(i, _)| i)
    };
    match key {
        "ArrowRight" => Some((current + 1).min(last)),
        "ArrowLeft" => Some(current.saturating_sub(1)),
        "ArrowDown" => vertical(true),
        "ArrowUp" => vertical(false),
        "Home" => Some(0),
        "End" => Some(last),
        _ => None,
    }
}

/// Bounded single-line preview for the polite new-message announcement in
/// the focused channel; never the full body, and never spoiler text.
pub fn preview(body: &str, limit: usize) -> String {
    let body = thiscord_shared::chat::mask_spoilers(body);
    let flattened = body.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut preview: String = flattened.chars().take(limit).collect();
    if flattened.chars().count() > limit {
        preview.push('…');
    }
    preview
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(key: &'static str, code: &'static str) -> Keys<'static> {
        Keys {
            key,
            code,
            ..Default::default()
        }
    }

    #[test]
    fn commands_need_modifiers_and_exact_chords() {
        let mut k = keys("k", "KeyK");
        assert_eq!(Command::from_keys(&k, false), None);
        k.control = true;
        assert_eq!(Command::from_keys(&k, false), Some(Command::QuickSwitcher));
        k.meta = true;
        assert_eq!(
            Command::from_keys(&k, false),
            None,
            "Ctrl+Cmd is not a chord"
        );
        k.control = false;
        assert_eq!(Command::from_keys(&k, false), Some(Command::QuickSwitcher));
        k.shift = true;
        assert_eq!(Command::from_keys(&k, false), None);
        // Non-Latin layouts still report the physical key.
        let mut cyrillic = keys("л", "KeyK");
        cyrillic.control = true;
        assert_eq!(
            Command::from_keys(&cyrillic, true),
            Some(Command::QuickSwitcher)
        );

        let mut slash = keys("/", "Digit7");
        slash.control = true;
        slash.shift = true;
        assert_eq!(
            Command::from_keys(&slash, true),
            Some(Command::ShortcutHelp)
        );

        // Ctrl+F is search; Ctrl+Shift+Space (push-to-talk) stays unclaimed.
        let mut f = keys("f", "KeyF");
        f.control = true;
        assert_eq!(Command::from_keys(&f, false), Some(Command::Search));
        let mut ptt = keys(" ", "Space");
        ptt.control = true;
        ptt.shift = true;
        assert_eq!(Command::from_keys(&ptt, false), None);
    }

    #[test]
    fn arrow_chords_map_to_navigation() {
        let keys = |key, control, alt, shift| Keys {
            key,
            code: key,
            control,
            alt,
            shift,
            meta: false,
        };
        let arrow =
            |key, control, alt, shift| Command::from_keys(&keys(key, control, alt, shift), false);
        assert_eq!(arrow("ArrowUp", false, false, false), None, "plain ↑ edits");
        assert_eq!(
            arrow("ArrowUp", false, true, false),
            Some(Command::PreviousChannel)
        );
        assert_eq!(
            arrow("ArrowDown", false, true, false),
            Some(Command::NextChannel)
        );
        assert_eq!(
            arrow("ArrowUp", false, true, true),
            Some(Command::PreviousUnread)
        );
        assert_eq!(
            arrow("ArrowDown", false, true, true),
            Some(Command::NextUnread)
        );
        assert_eq!(
            arrow("ArrowUp", true, true, false),
            Some(Command::PreviousServer)
        );
        assert_eq!(
            arrow("ArrowDown", true, false, true),
            Some(Command::NextServer)
        );
        assert_eq!(arrow("ArrowDown", true, true, true), None);
        assert_eq!(arrow("ArrowLeft", false, true, false), None);
        // While typing, Ctrl+Shift+↑/↓ keeps selecting text; Ctrl+Alt still works.
        let typing =
            |key, control, alt, shift| Command::from_keys(&keys(key, control, alt, shift), true);
        assert_eq!(typing("ArrowDown", true, false, true), None);
        assert_eq!(
            typing("ArrowDown", true, true, false),
            Some(Command::NextServer)
        );
        assert_eq!(
            typing("ArrowDown", false, true, false),
            Some(Command::NextChannel)
        );
    }

    #[test]
    fn recorded_bindings_replace_defaults_and_their_aliases() {
        let press = |key, code, control, shift, alt| Keys {
            key,
            code,
            control,
            shift,
            alt,
            meta: false,
        };
        let mut custom = AppShortcuts {
            quick_switcher: "Alt+P".into(),
            next_server: "F6".into(),
            shortcut_help: "Control+Alt+H".into(),
            ..AppShortcuts::default()
        };
        assert_eq!(
            custom.command(&press("p", "KeyP", false, false, true), false),
            Some(Command::QuickSwitcher)
        );
        assert_eq!(
            custom.command(&press("k", "KeyK", true, false, false), false),
            None
        );
        assert_eq!(
            custom.command(&press("F6", "F6", false, false, false), true),
            Some(Command::NextServer)
        );
        // Aliases belong to defaults only: a moved binding releases them.
        assert_eq!(
            custom.command(&press("ArrowDown", "ArrowDown", true, true, false), false),
            None
        );
        assert_eq!(
            custom.command(&press("ArrowUp", "ArrowUp", true, true, false), false),
            Some(Command::PreviousServer)
        );
        assert_eq!(
            custom.command(&press("/", "Digit7", true, true, false), false),
            None
        );
        // Cleared commands never match.
        custom.search.clear();
        assert_eq!(
            custom.command(&press("f", "KeyF", true, false, false), false),
            None
        );
    }

    #[test]
    fn app_bindings_validate_safety_and_conflicts() {
        assert_eq!(AppShortcuts::default().validate(&[]), Ok(()));
        let with = |command: Command, text: &str| {
            let mut shortcuts = AppShortcuts::default();
            *shortcuts.get_mut(command) = text.into();
            shortcuts
        };
        for unsafe_key in ["K", "Shift+K", "ArrowUp", "Enter", "Escape"] {
            assert_eq!(
                with(Command::QuickSwitcher, unsafe_key).validate(&[]),
                Err(COMMAND_KEY),
                "{unsafe_key}"
            );
        }
        assert!(with(Command::QuickSwitcher, "F2").validate(&[]).is_ok());
        assert!(with(Command::QuickSwitcher, "").validate(&[]).is_ok());
        assert_eq!(
            with(Command::QuickSwitcher, "Ctrl+F").validate(&[]),
            Err("Each action needs a different shortcut")
        );
        assert_eq!(
            AppShortcuts::default().validate(&["Control+K"]),
            Err("Each action needs a different shortcut")
        );
        assert!(
            AppShortcuts::default()
                .validate(&["Control+Shift+M", "", "Control+Shift+Space"])
                .is_ok()
        );
        assert!(with(Command::Search, "nope+F").validate(&[]).is_err());
        let saved: AppShortcuts = serde_json::from_str(r#"{"quick_switcher":"Alt+P"}"#).unwrap();
        assert_eq!(saved.quick_switcher, "Alt+P");
        assert_eq!(saved.search, "CmdOrCtrl+F");
    }

    #[test]
    fn bindings_display_like_the_reference() {
        assert_eq!(display_binding("CmdOrCtrl+K", false), "Ctrl+K");
        assert_eq!(display_binding("CmdOrCtrl+K", true), "⌘K");
        assert_eq!(display_binding("Alt+Shift+ArrowUp", false), "Alt+Shift+↑");
        assert_eq!(display_binding("Control+Alt+ArrowDown", true), "⌃⌥↓");
        assert_eq!(display_binding("Super+Shift+K", true), "⌘⇧K");
        assert_eq!(display_binding("", false), "");
    }

    #[test]
    fn scores_prefer_exact_prefix_word_substring_then_fuzzy() {
        let exact = score("general", "general").unwrap();
        let prefix = score("gen", "general").unwrap();
        let word = score("chat", "voice-chat").unwrap();
        let inner = score("era", "general").unwrap();
        let fuzzy = score("gnrl", "general").unwrap();
        assert!(exact > prefix && prefix > word && word > inner && inner > fuzzy);
        assert!(fuzzy > 0);
        assert_eq!(score("xyz", "general"), None);
        assert_eq!(score("lareneg", "general"), None, "order matters");
        assert_eq!(score("", "anything"), Some(0));
        // Shorter names win among prefixes; case and Turkish dotted I fold.
        assert!(score("dev", "dev").unwrap() > score("dev", "devops").unwrap());
        assert!(score("dev", "devops").unwrap() > score("dev", "development").unwrap());
        assert!(score("İSTANBUL", "istanbul").is_some());
        assert!(score("ılık", "Ilık").is_some());
        assert!(score("voice chat", "voice-chat").is_some());
    }

    fn candidate(kind: Kind, label: &str, context: &str, unread: bool) -> Candidate {
        Candidate {
            kind,
            label: label.into(),
            context: context.into(),
            unread,
            current: false,
        }
    }

    #[test]
    fn ranking_orders_by_score_then_unread_then_sidebar_order() {
        let items = vec![
            candidate(Kind::TextChannel, "development", "Rustaceans", false),
            candidate(Kind::TextChannel, "dev", "Rustaceans", false),
            candidate(Kind::Server, "Devs", "", true),
            candidate(Kind::Conversation, "Deniz", "", false),
            candidate(Kind::VoiceChannel, "lounge", "Rustaceans", false),
            candidate(Kind::TextChannel, "devops", "Ops", true),
        ];
        assert_eq!(rank("dev", &items, 10), vec![1, 2, 5, 0]);
        assert_eq!(rank("dev", &items, 2), vec![1, 2]);
        // Context (server name) finds channels with lower weight.
        assert_eq!(rank("rustaceans", &items, 10), vec![1, 4, 0]);
        assert!(rank("zzz", &items, 10).is_empty());
    }

    #[test]
    fn empty_query_lists_unread_first_and_current_last() {
        let mut items = vec![
            candidate(Kind::Server, "A", "", false),
            candidate(Kind::Server, "B", "", true),
            candidate(Kind::Server, "C", "", false),
        ];
        items[0].current = true;
        assert_eq!(rank("  ", &items, 10), vec![1, 2, 0]);
    }

    #[test]
    fn cycle_wraps_skips_ineligible_and_never_returns_current() {
        assert_eq!(cycle(0, None, true, |_| true), None);
        assert_eq!(cycle(4, Some(3), true, |_| true), Some(0));
        assert_eq!(cycle(4, Some(0), false, |_| true), Some(3));
        assert_eq!(cycle(4, None, true, |_| true), Some(0));
        assert_eq!(cycle(4, None, false, |_| true), Some(3));
        assert_eq!(cycle(5, Some(1), true, |i| i % 2 == 1), Some(3));
        assert_eq!(cycle(5, Some(3), true, |i| i % 2 == 1), Some(1));
        assert_eq!(cycle(5, Some(1), false, |i| i == 4), Some(4));
        assert_eq!(cycle(3, Some(1), true, |i| i == 1), None);
        assert_eq!(cycle(1, Some(0), true, |_| true), None);
        assert_eq!(cycle(3, Some(9), false, |_| true), Some(2));
    }

    #[test]
    fn list_and_spatial_steps() {
        assert_eq!(list_step(3, Some(2), "ArrowDown"), Some(0));
        assert_eq!(list_step(3, Some(0), "ArrowUp"), Some(2));
        assert_eq!(list_step(3, None, "ArrowDown"), Some(0));
        assert_eq!(list_step(1, Some(0), "ArrowDown"), Some(0));
        assert_eq!(list_step(3, Some(1), "Home"), Some(0));
        assert_eq!(list_step(3, Some(1), "End"), Some(2));
        assert_eq!(list_step(3, Some(1), "a"), None);
        assert_eq!(list_step(0, None, "Home"), None);

        // Two sections: 6 items in rows of 4, then 3 items; 40px cells.
        let mut centers = vec![];
        for i in 0..6 {
            centers.push((20.0 + 40.0 * (i % 4) as f64, 20.0 + 40.0 * (i / 4) as f64));
        }
        for i in 0..3 {
            centers.push((20.0 + 40.0 * i as f64, 130.0));
        }
        assert_eq!(spatial_step(&centers, 1, "ArrowDown"), Some(5));
        assert_eq!(
            spatial_step(&centers, 3, "ArrowDown"),
            Some(5),
            "closest column"
        );
        assert_eq!(spatial_step(&centers, 5, "ArrowDown"), Some(7));
        assert_eq!(
            spatial_step(&centers, 7, "ArrowDown"),
            Some(7),
            "bottom edge"
        );
        assert_eq!(spatial_step(&centers, 8, "ArrowUp"), Some(5));
        assert_eq!(spatial_step(&centers, 2, "ArrowUp"), Some(2), "top edge");
        assert_eq!(spatial_step(&centers, 3, "ArrowRight"), Some(4));
        assert_eq!(spatial_step(&centers, 8, "ArrowRight"), Some(8));
        assert_eq!(spatial_step(&centers, 0, "ArrowLeft"), Some(0));
        assert_eq!(spatial_step(&centers, 4, "Home"), Some(0));
        assert_eq!(spatial_step(&centers, 4, "End"), Some(8));
        assert_eq!(spatial_step(&centers, 4, "Tab"), None);
        assert_eq!(spatial_step(&[], 0, "ArrowDown"), None);
    }

    #[test]
    fn channel_order_matches_sidebar() {
        use thiscord_shared::permissions::ChannelKind;
        fn id<T: std::str::FromStr>(n: u8) -> T
        where
            T::Err: std::fmt::Debug,
        {
            format!("00000000-0000-0000-0000-{n:012}").parse().unwrap()
        }
        let channel = |n: u8, category: Option<u8>, position: i32| Channel {
            id: id(n),
            name: format!("c{n}"),
            kind: ChannelKind::Text,
            category_id: category.map(id),
            position,
        };
        let categories = vec![
            Category {
                id: id(50),
                name: "B".into(),
                position: 2,
            },
            Category {
                id: id(51),
                name: "A".into(),
                position: 1,
            },
        ];
        let channels = vec![
            channel(1, Some(50), 0),
            channel(2, Some(51), 1),
            channel(3, None, 5),
            channel(4, Some(51), 0),
            channel(5, Some(50), 1),
        ];
        let pinned = [id(5)].into_iter().collect();
        let names: Vec<_> = channel_order(&channels, &categories, &pinned)
            .into_iter()
            .map(|c| c.name.as_str())
            .collect();
        assert_eq!(names, ["c3", "c4", "c2", "c5", "c1"]);
    }

    #[test]
    fn previews_are_bounded_single_line() {
        assert_eq!(
            preview(
                "hi
there", 100
            ),
            "hi there"
        );
        assert_eq!(preview("abcdef", 3), "abc…");
        assert_eq!(preview("  ", 10), "");
        assert_eq!(preview(&"ğ".repeat(500), 140).chars().count(), 141);
        assert!(!preview("look ||the twist|| ok", 140).contains("twist"));
    }
}
