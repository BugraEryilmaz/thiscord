//! Device-local appearance preferences. They are presentation-only: values are
//! validated here before they reach CSS custom properties and never leave the
//! device unless the user copies a theme code.

/// Built-in starting points. A theme sets the background and text colors;
/// every color can then be customized individually.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Graphite,
    Midnight,
    Slate,
    Mocha,
    Forest,
    Daylight,
}

/// One customizable color. `Token::ALL` order is the order of
/// `Palette` entries and of the settings rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Token {
    Surface,
    Sidebar,
    Rail,
    Dock,
    Panel,
    Popover,
    Raised,
    Text,
    Muted,
    Link,
    Accent,
    Speaking,
    Success,
    Warning,
    Danger,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Group {
    /// Set by the chosen theme.
    Backgrounds,
    /// Set by the chosen theme.
    Text,
    /// Shared by every theme.
    Accents,
}

impl Group {
    pub const ALL: [Self; 3] = [Self::Backgrounds, Self::Text, Self::Accents];

    pub fn label(self) -> &'static str {
        match self {
            Self::Backgrounds => "Backgrounds",
            Self::Text => "Text",
            Self::Accents => "Accent and status",
        }
    }

    pub fn tokens(self) -> impl Iterator<Item = Token> {
        Token::ALL
            .into_iter()
            .filter(move |token| token.group() == self)
    }
}

impl Token {
    pub const COUNT: usize = 15;
    pub const ALL: [Self; Self::COUNT] = [
        Self::Surface,
        Self::Sidebar,
        Self::Rail,
        Self::Dock,
        Self::Panel,
        Self::Popover,
        Self::Raised,
        Self::Text,
        Self::Muted,
        Self::Link,
        Self::Accent,
        Self::Speaking,
        Self::Success,
        Self::Warning,
        Self::Danger,
    ];

    fn index(self) -> usize {
        self as usize
    }

    /// Stable identifier used in saved settings and theme codes.
    pub fn id(self) -> &'static str {
        match self {
            Self::Surface => "surface",
            Self::Sidebar => "sidebar",
            Self::Rail => "rail",
            Self::Dock => "dock",
            Self::Panel => "panel",
            Self::Popover => "popover",
            Self::Raised => "raised",
            Self::Text => "text",
            Self::Muted => "muted",
            Self::Accent => "accent",
            Self::Link => "link",
            Self::Speaking => "speaking",
            Self::Success => "success",
            Self::Warning => "warning",
            Self::Danger => "danger",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|token| token.id() == id)
    }

    /// CSS custom property declared in `style.css` `@theme`.
    pub fn variable(self) -> &'static str {
        match self {
            Self::Surface => "--color-surface",
            Self::Sidebar => "--color-sidebar",
            Self::Rail => "--color-rail",
            Self::Dock => "--color-dock",
            Self::Panel => "--color-panel",
            Self::Popover => "--color-popover",
            Self::Raised => "--color-raised",
            Self::Text => "--color-content",
            Self::Muted => "--color-muted",
            Self::Accent => "--color-brand",
            Self::Link => "--color-link",
            Self::Speaking => "--color-speaking",
            Self::Success => "--color-success",
            Self::Warning => "--color-warning",
            Self::Danger => "--color-danger",
        }
    }

    pub fn group(self) -> Group {
        match self {
            Self::Surface
            | Self::Sidebar
            | Self::Rail
            | Self::Dock
            | Self::Panel
            | Self::Popover
            | Self::Raised => Group::Backgrounds,
            Self::Text | Self::Muted | Self::Link => Group::Text,
            _ => Group::Accents,
        }
    }

    /// English label; translate it at the call site.
    pub fn label(self) -> &'static str {
        match self {
            Self::Surface => "Chat background",
            Self::Sidebar => "Channel list",
            Self::Rail => "Server list",
            Self::Dock => "Account panel",
            Self::Panel => "Cards",
            Self::Popover => "Menus",
            Self::Raised => "Server icons",
            Self::Text => "Text",
            Self::Muted => "Secondary text",
            Self::Accent => "Accent",
            Self::Link => "Links",
            Self::Speaking => "Speaking",
            Self::Success => "Online",
            Self::Warning => "Warning",
            Self::Danger => "Danger",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Surface => "Messages, dialogs and the main area.",
            Self::Sidebar => "Channels, direct messages and settings navigation.",
            Self::Rail => "The column of servers on the far left.",
            Self::Dock => "Your profile and voice controls at the bottom.",
            Self::Panel => "Settings sections and notifications.",
            Self::Popover => "Menus, pickers and tooltips.",
            Self::Raised => "Server buttons and rail actions.",
            Self::Text => "Messages, names and headings. Hover and border tints follow it.",
            Self::Muted => "Labels, hints and icons. Faded timestamps follow Text.",
            Self::Accent => "Buttons, selections, mentions and focus outlines.",
            Self::Link => "Links and replies in messages.",
            Self::Speaking => "Rings around people who are talking.",
            Self::Success => "Online status, voice connections and confirmations.",
            Self::Warning => "Connecting status and cautions.",
            Self::Danger => "Errors, deletions, Do Not Disturb and unread markers.",
        }
    }
}

/// One color for every token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette([Rgb; Token::COUNT]);

impl Palette {
    pub fn get(&self, token: Token) -> Rgb {
        self.0[token.index()]
    }
}

const fn hex(value: u32) -> Rgb {
    Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

pub const DEFAULT_ACCENT: Rgb = hex(0x5865f2);
/// Text, secondary text and links for dark themes.
const LIGHT_TEXT: [u32; 3] = [0xf2f3f5, 0xb7b9c4, 0x00bcff];
/// Accent and status colors shared by every theme.
const ACCENT_DEFAULTS: [Rgb; 5] = [
    DEFAULT_ACCENT,
    hex(0x54e697),
    hex(0x53bd84),
    hex(0xf4be58),
    hex(0xed4245),
];

/// Backgrounds in `Token` order, then text, secondary text and links.
const fn palette(backgrounds: [u32; 7], text: [u32; 3]) -> Palette {
    let mut colors = [Rgb(0, 0, 0); Token::COUNT];
    let mut i = 0;
    while i < 7 {
        colors[i] = hex(backgrounds[i]);
        i += 1;
    }
    while i < 10 {
        colors[i] = hex(text[i - 7]);
        i += 1;
    }
    while i < Token::COUNT {
        colors[i] = ACCENT_DEFAULTS[i - 10];
        i += 1;
    }
    Palette(colors)
}

impl Theme {
    pub const ALL: [Self; 6] = [
        Self::Graphite,
        Self::Midnight,
        Self::Slate,
        Self::Mocha,
        Self::Forest,
        Self::Daylight,
    ];

    /// Stable storage identifier.
    pub fn id(self) -> &'static str {
        match self {
            Self::Graphite => "graphite",
            Self::Midnight => "midnight",
            Self::Slate => "slate",
            Self::Mocha => "mocha",
            Self::Forest => "forest",
            Self::Daylight => "daylight",
        }
    }

    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|theme| theme.id() == id)
    }

    /// English label; translate it at the call site.
    pub fn label(self) -> &'static str {
        match self {
            Self::Graphite => "Graphite",
            Self::Midnight => "Midnight",
            Self::Slate => "Slate",
            Self::Mocha => "Mocha",
            Self::Forest => "Forest",
            Self::Daylight => "Daylight",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Graphite => "The classic Thiscord look.",
            Self::Midnight => "Near-black surfaces for OLED screens and dark rooms.",
            Self::Slate => "Cool blue-gray tones.",
            Self::Mocha => "Warm, coffee-brown tones.",
            Self::Forest => "Calm, deep green tones.",
            Self::Daylight => "Light surfaces with dark text.",
        }
    }

    /// Order: surface, sidebar, rail, dock, panel, popover, raised.
    pub const fn palette(self) -> Palette {
        match self {
            Self::Graphite => palette(
                [
                    0x202127, 0x191a21, 0x121318, 0x292a32, 0x24262e, 0x111218, 0x30313c,
                ],
                LIGHT_TEXT,
            ),
            Self::Midnight => palette(
                [
                    0x0e0e11, 0x0a0a0c, 0x000000, 0x17171b, 0x141418, 0x050506, 0x1f1f25,
                ],
                LIGHT_TEXT,
            ),
            Self::Slate => palette(
                [
                    0x1e2430, 0x181d27, 0x10141c, 0x262d3b, 0x222a37, 0x0e121a, 0x2d3544,
                ],
                LIGHT_TEXT,
            ),
            Self::Mocha => palette(
                [
                    0x241f1c, 0x1d1916, 0x14110f, 0x2e2724, 0x2a2320, 0x120f0d, 0x372f2a,
                ],
                LIGHT_TEXT,
            ),
            Self::Forest => palette(
                [
                    0x1b2420, 0x161e1a, 0x0e1512, 0x232e29, 0x1f2a25, 0x0b110e, 0x2b3832,
                ],
                LIGHT_TEXT,
            ),
            Self::Daylight => palette(
                [
                    0xffffff, 0xf2f3f5, 0xe3e5e8, 0xebedef, 0xf6f7f8, 0xffffff, 0xdcdee3,
                ],
                [0x23262b, 0x5c5f68, 0x0060c7],
            ),
        }
    }
}

/// An opaque sRGB color.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Rgb(pub u8, pub u8, pub u8);

impl Rgb {
    /// Accepts only `#rrggbb` (case-insensitive), the format produced by
    /// `<input type="color">`, so stored values cannot inject other CSS.
    pub fn parse(value: &str) -> Option<Self> {
        let digits = value.strip_prefix('#')?;
        if digits.len() != 6 || !digits.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        let channel = |i: usize| u8::from_str_radix(&digits[i..i + 2], 16).ok();
        Some(Self(channel(0)?, channel(2)?, channel(4)?))
    }

    /// Lowercase `#rrggbb`.
    pub fn hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.0, self.1, self.2)
    }

    /// `weight` of this color blended with `1 - weight` of `other`.
    pub fn mix(self, other: Self, weight: f64) -> Self {
        let channel = |a: u8, b: u8| {
            (f64::from(a) * weight + f64::from(b) * (1.0 - weight))
                .round()
                .clamp(0.0, 255.0) as u8
        };
        Self(
            channel(self.0, other.0),
            channel(self.1, other.1),
            channel(self.2, other.2),
        )
    }

    /// WCAG 2 relative luminance.
    pub fn luminance(self) -> f64 {
        let linear = |channel: u8| {
            let c = f64::from(channel) / 255.0;
            if c <= 0.039_28 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * linear(self.0) + 0.7152 * linear(self.1) + 0.0722 * linear(self.2)
    }

    /// WCAG 2 contrast ratio, from 1 to 21.
    pub fn contrast(self, other: Self) -> f64 {
        let (a, b) = (self.luminance(), other.luminance());
        (a.max(b) + 0.05) / (a.min(b) + 0.05)
    }

    /// White or near-black, whichever reads better on this color.
    pub fn on_color(self) -> Self {
        if WHITE.contrast(self) >= INK.contrast(self) {
            WHITE
        } else {
            INK
        }
    }
}

const WHITE: Rgb = Rgb(0xff, 0xff, 0xff);
const BLACK: Rgb = Rgb(0, 0, 0);
const INK: Rgb = Rgb(0x11, 0x12, 0x16);

/// Preset accents; labels are English and translated at the call site.
pub const ACCENTS: [(&str, Rgb); 8] = [
    ("Blurple", DEFAULT_ACCENT),
    ("Ocean", Rgb(0x25, 0x82, 0xeb)),
    ("Teal", Rgb(0x14, 0xa3, 0x94)),
    ("Green", Rgb(0x2f, 0xa8, 0x5a)),
    ("Amber", Rgb(0xf0, 0xa0, 0x20)),
    ("Coral", Rgb(0xf0, 0x5d, 0x4f)),
    ("Pink", Rgb(0xe0, 0x4f, 0x9b)),
    ("Violet", Rgb(0x90, 0x61, 0xf9)),
];

/// WCAG minimum for body text.
pub const MIN_TEXT_CONTRAST: f64 = 4.5;
/// WCAG minimum for large text and interface components; also used for
/// secondary text, accents and links.
pub const MIN_ACCENT_CONTRAST: f64 = 3.0;

/// Pairs checked for readability: foreground, background and minimum ratio.
const READABILITY: [(Token, Token, f64); 11] = [
    (Token::Text, Token::Surface, MIN_TEXT_CONTRAST),
    (Token::Text, Token::Sidebar, MIN_TEXT_CONTRAST),
    (Token::Text, Token::Dock, MIN_TEXT_CONTRAST),
    (Token::Text, Token::Panel, MIN_TEXT_CONTRAST),
    (Token::Text, Token::Popover, MIN_TEXT_CONTRAST),
    (Token::Text, Token::Raised, MIN_ACCENT_CONTRAST),
    (Token::Muted, Token::Surface, MIN_ACCENT_CONTRAST),
    (Token::Muted, Token::Sidebar, MIN_ACCENT_CONTRAST),
    (Token::Accent, Token::Surface, MIN_ACCENT_CONTRAST),
    (Token::Link, Token::Surface, MIN_ACCENT_CONTRAST),
    (Token::Danger, Token::Surface, MIN_ACCENT_CONTRAST),
];

/// Longest theme code accepted when importing or loading.
pub const MAX_CODE_LENGTH: usize = 512;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Appearance {
    pub theme: Theme,
    /// Colors chosen by the user; `None` follows the theme.
    custom: [Option<Rgb>; Token::COUNT],
}

impl Appearance {
    pub fn new(theme: Theme) -> Self {
        Self {
            theme,
            ..Self::default()
        }
    }

    /// Unknown or malformed saved values fall back to the defaults.
    pub fn resolve(saved: Option<&str>) -> Self {
        saved.and_then(Self::decode).unwrap_or_default()
    }

    /// Reads the separate theme and accent values saved by client 0.1.53.
    pub fn legacy(theme: Option<&str>, accent: Option<&str>) -> Self {
        let theme = theme.and_then(Theme::from_id).unwrap_or_default();
        Self::new(theme).with(Token::Accent, accent.and_then(Rgb::parse))
    }

    pub fn color(&self, token: Token) -> Rgb {
        self.custom[token.index()].unwrap_or_else(|| self.theme.palette().get(token))
    }

    pub fn is_custom(&self, token: Token) -> bool {
        self.custom[token.index()].is_some()
    }

    pub fn has_custom_colors(&self) -> bool {
        self.custom.iter().any(Option::is_some)
    }

    /// Sets one color. Choosing the theme's own color clears the override,
    /// so later theme changes keep applying to it.
    pub fn with(mut self, token: Token, color: Option<Rgb>) -> Self {
        self.custom[token.index()] =
            color.filter(|color| *color != self.theme.palette().get(token));
        self
    }

    /// Switches theme, replacing custom background and text colors and keeping
    /// custom accent and status colors.
    pub fn with_theme(mut self, theme: Theme) -> Self {
        self.theme = theme;
        for token in Token::ALL {
            if token.group() != Group::Accents {
                self.custom[token.index()] = None;
            }
        }
        // Re-apply the remaining overrides against the new defaults.
        for token in Group::Accents.tokens() {
            self = self.with(token, self.custom[token.index()]);
        }
        self
    }

    pub fn accent(&self) -> Rgb {
        self.color(Token::Accent)
    }

    /// Whether the accent may be hard to read as text on this theme.
    pub fn low_contrast(&self) -> bool {
        self.accent().contrast(self.color(Token::Surface)) < MIN_ACCENT_CONTRAST
    }

    /// Color pairs that fall below their minimum contrast.
    pub fn hard_to_read(&self) -> Vec<(Token, Token)> {
        READABILITY
            .into_iter()
            .filter(|(fg, bg, min)| self.color(*fg).contrast(self.color(*bg)) < *min)
            .map(|(fg, bg, _)| (fg, bg))
            .collect()
    }

    /// Shareable text such as `graphite;accent=#2fa85a`. It is also the saved
    /// form.
    pub fn encode(&self) -> String {
        let mut code = self.theme.id().to_owned();
        for token in Token::ALL {
            if let Some(color) = self.custom[token.index()] {
                code.push(';');
                code.push_str(token.id());
                code.push('=');
                code.push_str(&color.hex());
            }
        }
        code
    }

    /// Strict parser for saved values and pasted theme codes: an unknown
    /// theme, unknown color name or malformed color rejects the whole code.
    pub fn decode(code: &str) -> Option<Self> {
        let code = code.trim();
        if code.len() > MAX_CODE_LENGTH {
            return None;
        }
        let mut parts = code.split(';').map(str::trim);
        let mut appearance = Self::new(Theme::from_id(parts.next()?)?);
        for part in parts.filter(|part| !part.is_empty()) {
            let (id, value) = part.split_once('=')?;
            let token = Token::from_id(id.trim())?;
            let color = Rgb::parse(value.trim())?;
            appearance = appearance.with(token, Some(color));
        }
        Some(appearance)
    }

    /// CSS custom properties for the document root. `None` removes the
    /// property so the stylesheet default applies. Tailwind palette entries
    /// used by older views are only remapped once a related color differs
    /// from the default theme, so the default look keeps Tailwind's shades.
    pub fn properties(&self) -> Vec<(&'static str, Option<String>)> {
        let base = Theme::default().palette();
        let color = |token| self.color(token);
        let changed = |tokens: &[Token]| tokens.iter().any(|&t| color(t) != base.get(t));
        let mut out: Vec<(&'static str, Option<String>)> = Token::ALL
            .into_iter()
            .map(|token| (token.variable(), Some(color(token).hex())))
            .collect();
        out.push((
            "--color-on-brand",
            Some(color(Token::Accent).on_color().hex()),
        ));

        let text = color(Token::Text);
        let text_changed = changed(&[Token::Text]);
        // Headings, hover text and translucent tints: the text color pushed
        // further from the background.
        let extreme = if text.luminance() >= color(Token::Surface).luminance() {
            WHITE
        } else {
            BLACK
        };
        let strong = text.mix(extreme, 0.5);
        let mut remap = |enabled: bool, entries: Vec<(&'static str, Rgb)>| {
            for (name, value) in entries {
                out.push((name, enabled.then(|| value.hex())));
            }
        };
        remap(text_changed, vec![("--color-white", strong)]);
        // On light themes, recessed fields and dimmed layers drawn with
        // translucent black would turn gray; soften black toward the surface.
        remap(
            extreme == BLACK,
            vec![("--color-black", text.mix(color(Token::Surface), 0.45))],
        );

        let muted = color(Token::Muted);
        let surface = color(Token::Surface);
        remap(
            changed(&[Token::Text, Token::Muted]),
            vec![
                ("--color-slate-100", text),
                ("--color-slate-300", text.mix(muted, 0.5)),
                ("--color-slate-400", muted),
                ("--color-slate-500", muted.mix(surface, 0.7)),
            ],
        );
        let raised = color(Token::Raised);
        remap(
            changed(&Group::Backgrounds.tokens().collect::<Vec<_>>()),
            vec![
                ("--color-slate-700", raised.mix(muted, 0.85)),
                ("--color-slate-800", raised),
                ("--color-slate-900", color(Token::Sidebar)),
                ("--color-slate-950", color(Token::Rail)),
                ("--color-zinc-900", color(Token::Popover)),
            ],
        );
        // Light tints move toward the text color so they stay readable on
        // light backgrounds too.
        let scale = |token: Token, steps: &[(&'static str, f64)]| {
            let base = color(token);
            steps
                .iter()
                .map(|&(name, weight)| {
                    let value = if weight >= 1.0 {
                        base.mix(BLACK, 2.0 - weight)
                    } else {
                        base.mix(strong, weight)
                    };
                    (name, value)
                })
                .collect::<Vec<_>>()
        };
        for (token, steps) in [
            (
                Token::Danger,
                &[
                    ("--color-red-200", 0.35),
                    ("--color-red-300", 0.55),
                    ("--color-red-400", 0.8),
                    ("--color-red-500", 1.0),
                    ("--color-red-700", 1.3),
                ][..],
            ),
            (
                Token::Warning,
                &[
                    ("--color-amber-200", 0.45),
                    ("--color-amber-300", 0.7),
                    ("--color-amber-500", 1.0),
                ][..],
            ),
            (
                Token::Success,
                &[
                    ("--color-emerald-300", 0.6),
                    ("--color-emerald-400", 0.8),
                    ("--color-emerald-600", 1.2),
                    ("--color-emerald-900", 1.65),
                    ("--color-emerald-950", 1.8),
                ][..],
            ),
            (
                Token::Link,
                &[("--color-sky-300", 0.7), ("--color-sky-400", 1.0)][..],
            ),
            (
                Token::Accent,
                &[
                    ("--color-indigo-100", 0.2),
                    ("--color-indigo-300", 0.55),
                    ("--color-indigo-400", 0.75),
                    ("--color-indigo-500", 1.0),
                    ("--color-indigo-600", 1.15),
                ][..],
            ),
        ] {
            remap(changed(&[token, Token::Text]), scale(token, steps));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_six_digit_hex() {
        assert_eq!(Rgb::parse("#5865F2"), Some(DEFAULT_ACCENT));
        assert_eq!(DEFAULT_ACCENT.hex(), "#5865f2");
        for bad in [
            "",
            "5865f2",
            "#fff",
            "#5865f2ff",
            "#56xx f2",
            "#5865f2;color:red",
            "#٥٨٦٥f2",
            "red",
        ] {
            assert_eq!(Rgb::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn token_order_matches_palette_layout() {
        for (i, token) in Token::ALL.into_iter().enumerate() {
            assert_eq!(token.index(), i);
            assert_eq!(Token::from_id(token.id()), Some(token));
        }
        let graphite = Theme::Graphite.palette();
        assert_eq!(graphite.get(Token::Surface), hex(0x202127));
        assert_eq!(graphite.get(Token::Raised), hex(0x30313c));
        assert_eq!(graphite.get(Token::Text), hex(0xf2f3f5));
        assert_eq!(graphite.get(Token::Muted), hex(0xb7b9c4));
        assert_eq!(graphite.get(Token::Accent), DEFAULT_ACCENT);
        assert_eq!(graphite.get(Token::Danger), hex(0xed4245));
    }

    #[test]
    fn theme_codes_round_trip_and_reject_bad_input() {
        assert_eq!(Appearance::resolve(None), Appearance::default());
        assert_eq!(Appearance::default().encode(), "graphite");
        let custom = Appearance::new(Theme::Slate)
            .with(Token::Accent, Some(hex(0x14a394)))
            .with(Token::Surface, Some(hex(0x101010)));
        let code = custom.encode();
        assert_eq!(code, "slate;surface=#101010;accent=#14a394");
        assert_eq!(Theme::Graphite.palette().get(Token::Link), hex(0x00bcff));
        assert_eq!(Appearance::decode(&code), Some(custom));
        assert_eq!(
            Appearance::decode(" slate ; ACCENT=#14A394 ;\n"),
            None,
            "color names are case-sensitive"
        );
        assert_eq!(
            Appearance::decode(" slate ; accent=#14A394 ;\n"),
            Some(Appearance::new(Theme::Slate).with(Token::Accent, Some(hex(0x14a394))))
        );
        for bad in [
            "",
            "unknown",
            "graphite;accent",
            "graphite;accent=red",
            "graphite;accent=#5865f2;color:red",
            "graphite;background=#000000",
            "graphite;accent=#5865f2}body{display:none",
        ] {
            assert_eq!(Appearance::decode(bad), None, "{bad}");
        }
        let long = format!("graphite{}", ";accent=#5865f3".repeat(40));
        assert_eq!(Appearance::decode(&long), None);
        assert_eq!(Appearance::resolve(Some("unknown")), Appearance::default());
    }

    #[test]
    fn reads_settings_saved_by_the_first_appearance_release() {
        assert_eq!(Appearance::legacy(None, None), Appearance::default());
        assert_eq!(
            Appearance::legacy(Some("slate"), Some("#14A394")).encode(),
            "slate;accent=#14a394"
        );
        assert_eq!(
            Appearance::legacy(Some("unknown"), Some("red")),
            Appearance::default()
        );
    }

    #[test]
    fn theme_defaults_are_not_stored_as_custom_colors() {
        let picked = Appearance::default().with(Token::Accent, Some(DEFAULT_ACCENT));
        assert!(!picked.is_custom(Token::Accent));
        let switched = Appearance::default()
            .with(Token::Surface, Some(hex(0x000000)))
            .with(Token::Accent, Some(hex(0xf0a020)))
            .with_theme(Theme::Mocha);
        assert_eq!(switched.theme, Theme::Mocha);
        assert!(
            !switched.is_custom(Token::Surface),
            "theme replaces backgrounds"
        );
        assert_eq!(
            switched.accent(),
            hex(0xf0a020),
            "accent survives theme changes"
        );
    }

    #[test]
    fn default_appearance_matches_the_stylesheet() {
        // style.css @theme declares the Graphite palette and default accents.
        let css = include_str!("../style.css");
        for (name, value) in Appearance::default().properties() {
            match value {
                Some(value) => assert!(
                    css.contains(&format!("{name}: {value};")),
                    "style.css @theme is missing {name}: {value}"
                ),
                None => assert!(!css.contains(&format!("{name}:")), "{name}"),
            }
        }
    }

    #[test]
    fn tailwind_shades_are_only_remapped_when_needed() {
        let set = |appearance: Appearance| {
            appearance
                .properties()
                .into_iter()
                .filter(|(_, value)| value.is_some())
                .map(|(name, _)| name)
                .collect::<Vec<_>>()
        };
        assert!(!set(Appearance::default()).contains(&"--color-red-300"));
        let danger = Appearance::default().with(Token::Danger, Some(hex(0xff0000)));
        assert!(set(danger).contains(&"--color-red-300"));
        assert!(!set(danger).contains(&"--color-white"));
        let light = Appearance::new(Theme::Daylight);
        let properties = light.properties();
        let white = properties
            .iter()
            .find(|(name, _)| *name == "--color-white")
            .and_then(|(_, value)| value.clone())
            .unwrap();
        assert!(Rgb::parse(&white).unwrap().luminance() < 0.05, "{white}");
        assert!(set(light).contains(&"--color-slate-900"));
        assert!(set(light).contains(&"--color-black"));
        assert!(!set(Appearance::new(Theme::Midnight)).contains(&"--color-black"));
    }

    #[test]
    fn text_on_accent_picks_the_readable_color() {
        assert_eq!(DEFAULT_ACCENT.on_color(), WHITE);
        assert_eq!(Rgb(0xf0, 0xa0, 0x20).on_color(), INK);
        assert_eq!(Rgb(0xff, 0xff, 0xff).on_color(), INK);
        assert_eq!(Rgb(0, 0, 0).on_color(), WHITE);
        for (_, accent) in ACCENTS {
            let on = accent.on_color();
            assert!(accent.contrast(on) >= 4.5, "{}", accent.hex());
        }
        assert!((WHITE.contrast(Rgb(0, 0, 0)) - 21.0).abs() < 1e-9);
        assert_eq!(hex(0x204060).mix(WHITE, 0.0), WHITE);
        assert_eq!(hex(0x204060).mix(BLACK, 1.0), hex(0x204060));
    }

    #[test]
    fn every_theme_is_readable() {
        for theme in Theme::ALL {
            let appearance = Appearance::new(theme);
            assert_eq!(appearance.hard_to_read(), [], "{}", theme.id());
            let p = theme.palette();
            for surface in [Token::Surface, Token::Sidebar, Token::Panel, Token::Popover] {
                assert!(p.get(Token::Text).contrast(p.get(surface)) >= 7.0);
            }
        }
        for theme in Theme::ALL.into_iter().filter(|t| *t != Theme::Daylight) {
            for (label, accent) in ACCENTS {
                let appearance = Appearance::new(theme).with(Token::Accent, Some(accent));
                assert!(!appearance.low_contrast(), "{label} on {}", theme.id());
            }
        }
        let dark = Appearance::new(Theme::Midnight).with(Token::Accent, Some(hex(0x202040)));
        assert!(dark.low_contrast());
        assert!(
            dark.hard_to_read()
                .contains(&(Token::Accent, Token::Surface))
        );
        let unreadable = Appearance::default().with(Token::Text, Some(hex(0x303030)));
        assert!(
            unreadable
                .hard_to_read()
                .contains(&(Token::Text, Token::Surface))
        );
    }
}
