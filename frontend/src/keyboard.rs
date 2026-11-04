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

/// App-wide navigation commands. Each has Control/Command or Alt, so all of
/// them are safe while typing in the composer (like Discord).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    QuickSwitcher,
    ShortcutHelp,
    PreviousChannel,
    NextChannel,
    PreviousUnread,
    NextUnread,
    PreviousServer,
    NextServer,
}

impl Command {
    /// The command for a key press. Control and Command (Meta) are both the
    /// primary modifier on every platform; pressing both never matches.
    /// `typing` (focus in a text field) disables only the Ctrl+Shift+↑/↓
    /// fallback, which text fields use to select by paragraph.
    pub fn from_keys(keys: &Keys, typing: bool) -> Option<Self> {
        let primary = keys.control != keys.meta;
        let vertical = match keys.key {
            "ArrowUp" => Some(false),
            "ArrowDown" => Some(true),
            _ => None,
        };
        if let Some(forward) = vertical {
            let pick = |back, next| Some(if forward { next } else { back });
            return match (keys.control, keys.meta, keys.alt, keys.shift) {
                (false, false, true, false) => pick(Self::PreviousChannel, Self::NextChannel),
                (false, false, true, true) => pick(Self::PreviousUnread, Self::NextUnread),
                // Ctrl+Alt is the documented chord; Ctrl+Shift is the fallback
                // where the OS reserves Ctrl+Alt+Arrow (display rotation).
                (true, false, true, false) => pick(Self::PreviousServer, Self::NextServer),
                (true, false, false, true) if !typing => {
                    pick(Self::PreviousServer, Self::NextServer)
                }
                _ => None,
            };
        }
        if !primary || keys.alt {
            return None;
        }
        if keys.code == "KeyK" && !keys.shift {
            return Some(Self::QuickSwitcher);
        }
        // Layouts such as Turkish Q produce "/" with Shift; accept either key.
        if keys.code == "Slash" || keys.key == "/" {
            return Some(Self::ShortcutHelp);
        }
        None
    }
}

/// One row in the keyboard shortcut reference: key chords (already in display
/// form, joined with " / " for alternatives) and an untranslated description.
pub struct Shortcut {
    pub keys: &'static str,
    pub action: &'static str,
}

/// Groups of shortcuts for the help dialog and settings. Descriptions are
/// catalog keys for `i18n::t`.
pub const SHORTCUTS: &[(&str, &[Shortcut])] = &[
    (
        "Navigation",
        &[
            Shortcut {
                keys: "Ctrl+K",
                action: "Open the quick switcher",
            },
            Shortcut {
                keys: "Alt+↑ / Alt+↓",
                action: "Previous or next text channel",
            },
            Shortcut {
                keys: "Alt+Shift+↑ / Alt+Shift+↓",
                action: "Previous or next unread channel",
            },
            Shortcut {
                keys: "Ctrl+Alt+↑ / Ctrl+Alt+↓",
                action: "Previous or next server (Ctrl+Shift+↑ / ↓ also works)",
            },
            Shortcut {
                keys: "Ctrl+/",
                action: "Show keyboard shortcuts",
            },
        ],
    ),
    (
        "Chat",
        &[
            Shortcut {
                keys: "Esc",
                action: "Jump to latest and mark the channel read; closes menus and dialogs first",
            },
            Shortcut {
                keys: "Ctrl+F",
                action: "Search this conversation",
            },
            Shortcut {
                keys: "↑",
                action: "Edit your last message (empty composer)",
            },
            Shortcut {
                keys: "Enter / Shift+Enter",
                action: "Send message / new line",
            },
        ],
    ),
    (
        "Lists and menus",
        &[
            Shortcut {
                keys: "↑ ↓ Home End",
                action: "Move between servers, channels, members, friends and menu items",
            },
            Shortcut {
                keys: "← → ↑ ↓",
                action: "Move in emoji and reaction grids",
            },
            Shortcut {
                keys: "Shift+F10 / Menu",
                action: "Open the context menu for the focused item",
            },
            Shortcut {
                keys: "Enter / Space",
                action: "Activate the focused item",
            },
        ],
    ),
    (
        "Voice",
        &[Shortcut {
            keys: "Ctrl+Shift+Space",
            action: "Push to talk (hold)",
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
/// the focused channel; never the full body.
pub fn preview(body: &str, limit: usize) -> String {
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

        // Ctrl+F (search) and Ctrl+Shift+Space (push-to-talk) stay unclaimed.
        let mut f = keys("f", "KeyF");
        f.control = true;
        assert_eq!(Command::from_keys(&f, false), None);
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
    }
}
