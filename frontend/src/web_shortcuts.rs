//! Focused-page keyboard shortcuts for the browser web client. Bindings use the
//! desktop's text form (`Control+Shift+M`, `Alt+M`, `F8`) and are matched
//! against `KeyboardEvent.code`, so they follow physical keys like the native
//! global shortcuts. Browsers deliver keys only while the page has focus.
use thiscord_shared::audio::{AudioShortcuts, AudioToggle};

/// One parsed key combination.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
    /// `CmdOrCtrl`: either Control or Meta.
    pub command_or_control: bool,
    /// A `KeyboardEvent.code` value.
    pub code: String,
}

/// The modifier and key state of a keyboard event.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Pressed<'a> {
    pub code: &'a str,
    pub control: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

/// Why a plain typing key cannot be push-to-talk (see [`Chord::blocks_typing`]).
pub const TYPING_KEY: &str = "Add Ctrl, Alt or Super to letter, number, space and punctuation keys so they still type normally.";

fn key_code(key: &str) -> Option<String> {
    let upper = key.to_ascii_uppercase();
    if upper.len() == 1 {
        let c = upper.chars().next()?;
        if c.is_ascii_uppercase() {
            return Some(format!("Key{c}"));
        }
        if c.is_ascii_digit() {
            return Some(format!("Digit{c}"));
        }
        let named = match c {
            '-' => "Minus",
            '=' => "Equal",
            ',' => "Comma",
            '.' => "Period",
            '/' => "Slash",
            '\\' => "Backslash",
            ';' => "Semicolon",
            '\'' => "Quote",
            '`' => "Backquote",
            '[' => "BracketLeft",
            ']' => "BracketRight",
            _ => return None,
        };
        return Some(named.into());
    }
    if let Some(number) = upper.strip_prefix('F')
        && let Ok(n) = number.parse::<u8>()
        && (1..=24).contains(&n)
    {
        return Some(format!("F{n}"));
    }
    if let Some(rest) = upper.strip_prefix("KEY")
        && rest.len() == 1
        && rest.chars().all(|c| c.is_ascii_uppercase())
    {
        return Some(format!("Key{rest}"));
    }
    if let Some(rest) = upper.strip_prefix("DIGIT")
        && rest.len() == 1
        && rest.chars().all(|c| c.is_ascii_digit())
    {
        return Some(format!("Digit{rest}"));
    }
    if let Some(rest) = upper.strip_prefix("NUMPAD") {
        if rest.len() == 1 && rest.chars().all(|c| c.is_ascii_digit()) {
            return Some(format!("Numpad{rest}"));
        }
        let named = match rest {
            "ADD" => "NumpadAdd",
            "SUBTRACT" => "NumpadSubtract",
            "MULTIPLY" => "NumpadMultiply",
            "DIVIDE" => "NumpadDivide",
            "DECIMAL" => "NumpadDecimal",
            "ENTER" => "NumpadEnter",
            _ => return None,
        };
        return Some(named.into());
    }
    let named = match upper.as_str() {
        "SPACE" => "Space",
        "ENTER" | "RETURN" => "Enter",
        "TAB" => "Tab",
        "ESCAPE" | "ESC" => "Escape",
        "BACKSPACE" => "Backspace",
        "DELETE" | "DEL" => "Delete",
        "INSERT" => "Insert",
        "HOME" => "Home",
        "END" => "End",
        "PAGEUP" => "PageUp",
        "PAGEDOWN" => "PageDown",
        "UP" | "ARROWUP" => "ArrowUp",
        "DOWN" | "ARROWDOWN" => "ArrowDown",
        "LEFT" | "ARROWLEFT" => "ArrowLeft",
        "RIGHT" | "ARROWRIGHT" => "ArrowRight",
        "MINUS" => "Minus",
        "EQUAL" => "Equal",
        "COMMA" => "Comma",
        "PERIOD" => "Period",
        "SLASH" => "Slash",
        "BACKSLASH" => "Backslash",
        "SEMICOLON" => "Semicolon",
        "QUOTE" => "Quote",
        "BACKQUOTE" => "Backquote",
        "BRACKETLEFT" => "BracketLeft",
        "BRACKETRIGHT" => "BracketRight",
        "PAUSE" => "Pause",
        "SCROLLLOCK" => "ScrollLock",
        "PRINTSCREEN" => "PrintScreen",
        _ => return None,
    };
    Some(named.into())
}

impl Chord {
    /// Parses the desktop's binding syntax; `None` for invalid text.
    pub fn parse(text: &str) -> Option<Self> {
        let mut chord = Self {
            control: false,
            shift: false,
            alt: false,
            meta: false,
            command_or_control: false,
            code: String::new(),
        };
        let parts: Vec<&str> = text.split('+').map(str::trim).collect();
        let (key, modifiers) = parts.split_last()?;
        for modifier in modifiers {
            let flag = match modifier.to_ascii_uppercase().as_str() {
                "CONTROL" | "CTRL" => &mut chord.control,
                "SHIFT" => &mut chord.shift,
                "ALT" | "OPTION" => &mut chord.alt,
                "SUPER" | "META" | "CMD" | "COMMAND" => &mut chord.meta,
                "CMDORCTRL" | "COMMANDORCONTROL" | "CMDORCONTROL" | "COMMANDORCTRL" => {
                    &mut chord.command_or_control
                }
                _ => return None,
            };
            if std::mem::replace(flag, true) {
                return None;
            }
        }
        chord.code = key_code(key)?;
        Some(chord)
    }
    pub fn matches(&self, pressed: &Pressed) -> bool {
        let (control, meta) = if self.command_or_control {
            // Either one, but not both and not in addition to explicit flags.
            if self.control || self.meta || pressed.control == pressed.meta {
                return false;
            }
            (pressed.control, pressed.meta)
        } else {
            (self.control, self.meta)
        };
        pressed.code == self.code
            && pressed.control == control
            && pressed.meta == meta
            && pressed.shift == self.shift
            && pressed.alt == self.alt
    }
    /// Whether this chord is safe to act on while the user types in a text
    /// field: it needs Control, Alt or Meta, or is a function key.
    pub fn while_typing(&self) -> bool {
        self.control
            || self.alt
            || self.meta
            || self.command_or_control
            || (self.code.starts_with('F') && self.code[1..].parse::<u8>().is_ok())
    }
    /// A letter, number, space or punctuation key without Control, Alt or
    /// Super. Global shortcuts consume their keys in every app, and a held
    /// push-to-talk key would swallow typing, so these are not offered.
    pub fn blocks_typing(&self) -> bool {
        !(self.control || self.alt || self.meta || self.command_or_control)
            && (self.code.starts_with("Key")
                || self.code.starts_with("Digit")
                || matches!(
                    self.code.as_str(),
                    "Space"
                        | "Enter"
                        | "Tab"
                        | "Backspace"
                        | "Minus"
                        | "Equal"
                        | "Comma"
                        | "Period"
                        | "Slash"
                        | "Backslash"
                        | "Semicolon"
                        | "Quote"
                        | "Backquote"
                        | "BracketLeft"
                        | "BracketRight"
                ))
    }
    /// Whether a released key ends this chord (for push-to-talk).
    pub fn released_by(&self, code: &str) -> bool {
        code == self.code
            || (self.control && code.starts_with("Control"))
            || (self.shift && code.starts_with("Shift"))
            || (self.alt && code.starts_with("Alt"))
            || (self.meta && (code.starts_with("Meta") || code.starts_with("OS")))
    }
    fn equivalent(&self, other: &Self) -> bool {
        let pressed = |control, meta| Pressed {
            code: &other.code,
            control,
            meta,
            shift: other.shift,
            alt: other.alt,
        };
        let variants = if other.command_or_control {
            vec![pressed(true, false), pressed(false, true)]
        } else {
            vec![pressed(other.control, other.meta)]
        };
        variants.iter().any(|p| self.matches(p))
    }
}

/// The result of one key press while recording a shortcut.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Recorded {
    /// Only modifiers so far; the held ones in binding syntax (may be empty).
    Pending(String),
    /// Escape without modifiers.
    Cancel,
    /// A complete combination in binding syntax, accepted by both the browser
    /// and the desktop parsers.
    Chord(String),
    /// A key neither parser supports (for example Caps Lock or media keys).
    Unsupported,
}

/// Turns a key press into binding text for the click-to-record control.
pub fn record(pressed: &Pressed) -> Recorded {
    let modifiers: Vec<&str> = [
        (pressed.control, "Control"),
        (pressed.alt, "Alt"),
        (pressed.shift, "Shift"),
        (pressed.meta, "Super"),
    ]
    .into_iter()
    .filter_map(|(held, name)| held.then_some(name))
    .collect();
    let code = pressed.code;
    if ["Control", "Shift", "Alt", "Meta", "OS"]
        .iter()
        .any(|m| code.starts_with(m))
    {
        return Recorded::Pending(modifiers.join("+"));
    }
    if code == "Escape" && modifiers.is_empty() {
        return Recorded::Cancel;
    }
    if key_code(code).as_deref() != Some(code) {
        return Recorded::Unsupported;
    }
    let key = code
        .strip_prefix("Key")
        .or_else(|| code.strip_prefix("Digit"))
        .unwrap_or(code);
    let text = modifiers
        .into_iter()
        .chain(std::iter::once(key))
        .collect::<Vec<_>>()
        .join("+");
    match Chord::parse(&text) {
        Some(_) => Recorded::Chord(text),
        None => Recorded::Unsupported,
    }
}

/// Short key-cap labels for showing a binding ("Ctrl", "Shift", "M").
pub fn labels(text: &str) -> Vec<String> {
    if text.trim().is_empty() {
        return Vec::new();
    }
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    let Some((key, modifiers)) = parts.split_last() else {
        return Vec::new();
    };
    let mut labels: Vec<String> = modifiers
        .iter()
        .map(|m| {
            match m.to_ascii_uppercase().as_str() {
                "CONTROL" | "CTRL" => "Ctrl",
                "SHIFT" => "Shift",
                "ALT" | "OPTION" => "Alt",
                "SUPER" | "META" | "CMD" | "COMMAND" => "Super",
                _ => "Ctrl/⌘",
            }
            .to_owned()
        })
        .collect();
    let code = key_code(key).unwrap_or_else(|| (*key).to_owned());
    let symbol = match code.as_str() {
        "ArrowUp" => Some("↑"),
        "ArrowDown" => Some("↓"),
        "ArrowLeft" => Some("←"),
        "ArrowRight" => Some("→"),
        "Backquote" => Some("`"),
        "Minus" => Some("-"),
        "Equal" => Some("="),
        "Comma" => Some(","),
        "Period" => Some("."),
        "Slash" => Some("/"),
        "Backslash" => Some("\\"),
        "Semicolon" => Some(";"),
        "Quote" => Some("'"),
        "BracketLeft" => Some("["),
        "BracketRight" => Some("]"),
        _ => None,
    };
    labels.push(match symbol {
        Some(symbol) => symbol.to_owned(),
        None => code
            .strip_prefix("Key")
            .or_else(|| code.strip_prefix("Digit"))
            .map(str::to_owned)
            .or_else(|| code.strip_prefix("Numpad").map(|n| format!("Num {n}")))
            .unwrap_or_else(|| code.clone()),
    });
    labels
}

/// Parsed browser bindings for push-to-talk and the self-control shortcuts.
#[derive(Debug, Clone, Default)]
pub struct Bindings {
    pub push_to_talk: Option<Chord>,
    pub mute: Option<Chord>,
    pub deafen: Option<Chord>,
}
impl Bindings {
    /// Validates like the desktop: bounded length, valid syntax, unique keys
    /// (push-to-talk included) and no plain typing key for push-to-talk.
    /// Overlay bindings are kept but unused.
    pub fn from_settings(settings: &AudioShortcuts) -> Result<Self, &'static str> {
        if settings.too_long() {
            return Err("Shortcut is too long");
        }
        let mut seen: Vec<Chord> = Vec::new();
        let mut parse = |text: &str| -> Result<Option<Chord>, &'static str> {
            if text.trim().is_empty() {
                return Ok(None);
            }
            let chord = Chord::parse(text)
                .ok_or("Invalid shortcut. Use a combination such as Control+Shift+M.")?;
            if seen
                .iter()
                .any(|other| other.equivalent(&chord) || chord.equivalent(other))
            {
                return Err("Each action needs a different shortcut");
            }
            seen.push(chord.clone());
            Ok(Some(chord))
        };
        let push_to_talk = parse(&settings.push_to_talk)?;
        if push_to_talk.as_ref().is_some_and(Chord::blocks_typing) {
            return Err(TYPING_KEY);
        }
        Ok(Self {
            push_to_talk,
            mute: parse(&settings.mute)?,
            deafen: parse(&settings.deafen)?,
        })
    }
    /// The toggle a key press triggers. In text fields only chords with a
    /// command modifier or function key apply, so typing is never captured.
    pub fn action(&self, pressed: &Pressed, typing: bool) -> Option<AudioToggle> {
        [
            (&self.mute, AudioToggle::Mute),
            (&self.deafen, AudioToggle::Deafen),
        ]
        .into_iter()
        .find_map(|(chord, action)| {
            chord
                .as_ref()
                .filter(|c| c.matches(pressed) && (!typing || c.while_typing()))
                .map(|_| action)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: &str) -> Pressed<'_> {
        Pressed {
            code,
            ..Default::default()
        }
    }

    #[test]
    fn parses_desktop_binding_syntax() {
        let chord = Chord::parse("Control+Shift+M").unwrap();
        assert!(chord.control && chord.shift && !chord.alt && !chord.meta);
        assert_eq!(chord.code, "KeyM");
        assert_eq!(Chord::parse("ctrl + alt + 1").unwrap().code, "Digit1");
        assert_eq!(Chord::parse("F8").unwrap().code, "F8");
        assert_eq!(Chord::parse("Super+KeyK").unwrap().code, "KeyK");
        assert!(Chord::parse("Super+K").unwrap().meta);
        assert_eq!(Chord::parse("Alt+Up").unwrap().code, "ArrowUp");
        assert_eq!(Chord::parse("Shift+Numpad5").unwrap().code, "Numpad5");
        for invalid in ["", "Control+", "Hyper+M", "Control+Control+M", "F25", "MM"] {
            assert!(Chord::parse(invalid).is_none(), "{invalid}");
        }
    }

    #[test]
    fn matching_requires_exact_modifiers() {
        let chord = Chord::parse("Control+Shift+M").unwrap();
        let mut pressed = Pressed {
            code: "KeyM",
            control: true,
            shift: true,
            ..Default::default()
        };
        assert!(chord.matches(&pressed));
        pressed.alt = true;
        assert!(!chord.matches(&pressed));
        assert!(!chord.matches(&press("KeyM")));
        let either = Chord::parse("CmdOrCtrl+K").unwrap();
        assert!(either.matches(&Pressed {
            code: "KeyK",
            meta: true,
            ..Default::default()
        }));
        assert!(either.matches(&Pressed {
            code: "KeyK",
            control: true,
            ..Default::default()
        }));
        assert!(!either.matches(&press("KeyK")));
    }

    #[test]
    fn typing_never_triggers_plain_keys() {
        let bindings = Bindings::from_settings(&AudioShortcuts {
            mute: "M".into(),
            deafen: "F9".into(),
            ..AudioShortcuts::default()
        })
        .unwrap();
        assert_eq!(
            bindings.action(&press("KeyM"), false),
            Some(AudioToggle::Mute)
        );
        assert_eq!(bindings.action(&press("KeyM"), true), None);
        assert_eq!(
            bindings.action(&press("F9"), true),
            Some(AudioToggle::Deafen)
        );
        let defaults = Bindings::from_settings(&AudioShortcuts::default()).unwrap();
        let ctrl_shift_d = Pressed {
            code: "KeyD",
            control: true,
            shift: true,
            ..Default::default()
        };
        assert_eq!(
            defaults.action(&ctrl_shift_d, true),
            Some(AudioToggle::Deafen)
        );
    }

    #[test]
    fn validation_matches_desktop_rules() {
        let with = |mute: &str, deafen: &str| {
            Bindings::from_settings(&AudioShortcuts {
                mute: mute.into(),
                deafen: deafen.into(),
                ..AudioShortcuts::default()
            })
        };
        assert!(with("", "").is_ok());
        // Push-to-talk joins the conflict check like any other action.
        assert_eq!(
            with("Ctrl+Shift+Space", "").unwrap_err(),
            "Each action needs a different shortcut"
        );
        assert_eq!(
            with("CmdOrCtrl+Shift+Space", "").unwrap_err(),
            "Each action needs a different shortcut"
        );
        assert_eq!(
            with("Alt+M", "alt+m").unwrap_err(),
            "Each action needs a different shortcut"
        );
        assert!(with("Nope+M", "").is_err());
        assert!(with(&"M".repeat(200), "").is_err());
    }

    #[test]
    fn push_to_talk_is_configurable_but_never_a_plain_typing_key() {
        let with = |ptt: &str| {
            Bindings::from_settings(&AudioShortcuts {
                push_to_talk: ptt.into(),
                ..AudioShortcuts::default()
            })
        };
        let f8 = with("F8").unwrap().push_to_talk.unwrap();
        assert!(f8.matches(&press("F8")));
        assert!(with("").unwrap().push_to_talk.is_none());
        assert!(with("Alt+V").is_ok());
        assert!(with("Numpad0").is_ok());
        for typing in ["V", "Space", "Shift+V", "Backquote", "Digit1"] {
            assert_eq!(with(typing).unwrap_err(), TYPING_KEY, "{typing}");
        }
        assert_eq!(
            with("Control+Shift+M").unwrap_err(),
            "Each action needs a different shortcut"
        );
        // The old fixed key is free for other actions once push-to-talk moves.
        assert!(
            Bindings::from_settings(&AudioShortcuts {
                push_to_talk: "F8".into(),
                mute: "Control+Shift+Space".into(),
                ..AudioShortcuts::default()
            })
            .is_ok()
        );
    }

    #[test]
    fn recording_produces_parseable_bindings() {
        let key = |code, control, shift, alt, meta| {
            record(&Pressed {
                code,
                control,
                shift,
                alt,
                meta,
            })
        };
        assert_eq!(
            key("KeyM", true, true, false, false),
            Recorded::Chord("Control+Shift+M".into())
        );
        assert_eq!(
            key("Digit1", false, false, true, false),
            Recorded::Chord("Alt+1".into())
        );
        assert_eq!(
            key("F8", false, false, false, false),
            Recorded::Chord("F8".into())
        );
        assert_eq!(
            key("ArrowUp", true, false, false, true),
            Recorded::Chord("Control+Super+ArrowUp".into())
        );
        assert_eq!(
            key("ShiftLeft", true, true, false, false),
            Recorded::Pending("Control+Shift".into())
        );
        assert_eq!(
            key("MetaLeft", false, false, false, true),
            Recorded::Pending("Super".into())
        );
        assert_eq!(key("Escape", false, false, false, false), Recorded::Cancel);
        assert_eq!(
            key("Escape", true, false, false, false),
            Recorded::Chord("Control+Escape".into())
        );
        assert_eq!(
            key("CapsLock", false, false, false, false),
            Recorded::Unsupported
        );
        assert_eq!(
            key("AudioVolumeUp", false, false, false, false),
            Recorded::Unsupported
        );
        // Every recorded binding round-trips through the parser.
        for code in [
            "KeyZ",
            "Digit0",
            "Space",
            "Numpad5",
            "Backquote",
            "F24",
            "PageDown",
        ] {
            let Recorded::Chord(text) = key(code, true, false, false, false) else {
                panic!("{code}");
            };
            let chord = Chord::parse(&text).unwrap();
            assert_eq!(chord.code, code);
            assert!(chord.matches(&Pressed {
                code,
                control: true,
                ..Default::default()
            }));
        }
        assert_eq!(labels("Control+Shift+M"), ["Ctrl", "Shift", "M"]);
        assert_eq!(labels("Super+ArrowUp"), ["Super", "↑"]);
        assert_eq!(labels("Numpad5"), ["Num 5"]);
        assert_eq!(labels("CmdOrCtrl+K"), ["Ctrl/⌘", "K"]);
        assert!(labels("").is_empty());
    }

    #[test]
    fn push_to_talk_releases_on_any_chord_key() {
        let ptt = Chord::parse(AudioShortcuts::DEFAULT_PUSH_TO_TALK).unwrap();
        for code in ["Space", "ControlLeft", "ShiftRight"] {
            assert!(ptt.released_by(code));
        }
        assert!(!ptt.released_by("KeyA"));
    }
}
