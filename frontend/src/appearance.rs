//! Device-local appearance preferences. They are presentation-only: values are
//! validated here before they reach CSS custom properties and never leave the
//! device.

/// Dark interface palettes. The UI draws translucent white overlays on these
/// surfaces, so every theme keeps light text on dark backgrounds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    #[default]
    Graphite,
    Midnight,
    Slate,
    Mocha,
    Forest,
}

/// Opaque surface colors for one theme.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Palette {
    /// Main content area and dialogs.
    pub surface: Rgb,
    /// Channel list and settings navigation.
    pub sidebar: Rgb,
    /// Server rail.
    pub rail: Rgb,
    /// Account and voice dock.
    pub dock: Rgb,
    /// Cards inside settings and notifications.
    pub panel: Rgb,
    /// Menus, popovers and tooltips.
    pub popover: Rgb,
    /// Idle server and action buttons on the rail.
    pub raised: Rgb,
}

impl Theme {
    pub const ALL: [Self; 5] = [
        Self::Graphite,
        Self::Midnight,
        Self::Slate,
        Self::Mocha,
        Self::Forest,
    ];

    /// Stable storage identifier.
    pub fn id(self) -> &'static str {
        match self {
            Self::Graphite => "graphite",
            Self::Midnight => "midnight",
            Self::Slate => "slate",
            Self::Mocha => "mocha",
            Self::Forest => "forest",
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
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            Self::Graphite => "The classic Thiscord look.",
            Self::Midnight => "Near-black surfaces for OLED screens and dark rooms.",
            Self::Slate => "Cool blue-gray tones.",
            Self::Mocha => "Warm, coffee-brown tones.",
            Self::Forest => "Calm, deep green tones.",
        }
    }

    pub const fn palette(self) -> Palette {
        const fn hex(value: u32) -> Rgb {
            Rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
        }
        match self {
            Self::Graphite => Palette {
                surface: hex(0x202127),
                sidebar: hex(0x191a21),
                rail: hex(0x121318),
                dock: hex(0x292a32),
                panel: hex(0x24262e),
                popover: hex(0x111218),
                raised: hex(0x30313c),
            },
            Self::Midnight => Palette {
                surface: hex(0x0e0e11),
                sidebar: hex(0x0a0a0c),
                rail: hex(0x000000),
                dock: hex(0x17171b),
                panel: hex(0x141418),
                popover: hex(0x050506),
                raised: hex(0x1f1f25),
            },
            Self::Slate => Palette {
                surface: hex(0x1e2430),
                sidebar: hex(0x181d27),
                rail: hex(0x10141c),
                dock: hex(0x262d3b),
                panel: hex(0x222a37),
                popover: hex(0x0e121a),
                raised: hex(0x2d3544),
            },
            Self::Mocha => Palette {
                surface: hex(0x241f1c),
                sidebar: hex(0x1d1916),
                rail: hex(0x14110f),
                dock: hex(0x2e2724),
                panel: hex(0x2a2320),
                popover: hex(0x120f0d),
                raised: hex(0x372f2a),
            },
            Self::Forest => Palette {
                surface: hex(0x1b2420),
                sidebar: hex(0x161e1a),
                rail: hex(0x0e1512),
                dock: hex(0x232e29),
                panel: hex(0x1f2a25),
                popover: hex(0x0b110e),
                raised: hex(0x2b3832),
            },
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
const INK: Rgb = Rgb(0x11, 0x12, 0x16);

pub const DEFAULT_ACCENT: Rgb = Rgb(0x58, 0x65, 0xf2);

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

/// Accent-on-surface contrast below this is flagged as hard to read; it is the
/// WCAG minimum for large text and interface components.
pub const MIN_ACCENT_CONTRAST: f64 = 3.0;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Appearance {
    pub theme: Theme,
    pub accent: Option<Rgb>,
}

impl Appearance {
    /// Unknown or malformed saved values fall back to the defaults.
    pub fn resolve(theme: Option<&str>, accent: Option<&str>) -> Self {
        Self {
            theme: theme.and_then(Theme::from_id).unwrap_or_default(),
            accent: accent.and_then(Rgb::parse),
        }
    }

    pub fn accent(self) -> Rgb {
        self.accent.unwrap_or(DEFAULT_ACCENT)
    }

    /// Whether the accent may be hard to read as text on this theme.
    pub fn low_contrast(self) -> bool {
        self.accent().contrast(self.theme.palette().surface) < MIN_ACCENT_CONTRAST
    }

    /// CSS custom properties set on the document root. Derived accent shades
    /// are computed in the stylesheet from `--color-brand`.
    pub fn properties(self) -> [(&'static str, String); 9] {
        let p = self.theme.palette();
        let accent = self.accent();
        [
            ("--color-surface", p.surface.hex()),
            ("--color-sidebar", p.sidebar.hex()),
            ("--color-rail", p.rail.hex()),
            ("--color-dock", p.dock.hex()),
            ("--color-panel", p.panel.hex()),
            ("--color-popover", p.popover.hex()),
            ("--color-raised", p.raised.hex()),
            ("--color-brand", accent.hex()),
            ("--color-on-brand", accent.on_color().hex()),
        ]
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
    fn resolve_falls_back_for_unknown_values() {
        assert_eq!(Appearance::resolve(None, None), Appearance::default());
        assert_eq!(
            Appearance::resolve(Some("unknown"), Some("javascript:")),
            Appearance::default()
        );
        let saved = Appearance::resolve(Some("slate"), Some("#14A394"));
        assert_eq!(saved.theme, Theme::Slate);
        assert_eq!(saved.accent(), Rgb(0x14, 0xa3, 0x94));
        for theme in Theme::ALL {
            assert_eq!(Theme::from_id(theme.id()), Some(theme));
        }
    }

    #[test]
    fn default_appearance_matches_the_stylesheet() {
        // style.css @theme declares the Graphite palette and Blurple accent.
        let css = include_str!("../style.css");
        for (name, value) in Appearance::default().properties() {
            assert!(
                css.contains(&format!("{name}: {value};")),
                "style.css @theme is missing {name}: {value}"
            );
        }
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
    }

    #[test]
    fn preset_accents_remain_readable_on_every_theme() {
        for theme in Theme::ALL {
            for (label, accent) in ACCENTS {
                let appearance = Appearance {
                    theme,
                    accent: Some(accent),
                };
                assert!(!appearance.low_contrast(), "{label} on {}", theme.id());
            }
            let p = theme.palette();
            // Content text (#f2f3f5) must stay readable on every surface.
            for surface in [p.surface, p.sidebar, p.panel, p.popover, p.dock] {
                assert!(Rgb(0xf2, 0xf3, 0xf5).contrast(surface) >= 7.0);
            }
        }
        let dark = Appearance {
            theme: Theme::Midnight,
            accent: Some(Rgb(0x20, 0x20, 0x40)),
        };
        assert!(dark.low_contrast());
    }
}
