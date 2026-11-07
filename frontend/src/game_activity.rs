//! Platform-neutral game detection for the desktop "Playing …" status.
//!
//! The desktop app lists running process names locally and matches them against
//! this built-in table. Process names never leave the device; only the matched
//! display name is sent, and only while sharing is enabled (see docs/activity.md).
use serde::{Deserialize, Serialize};
use thiscord_shared::account::{Activity, MAX_ACTIVITY_CHARS};

/// Desktop event carrying an [`ActivityStatus`] whenever either field changes.
pub const ACTIVITY_EVENT: &str = "activity-status";

/// Device-local detection state shared between the native app and the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityStatus {
    /// Detection and sharing are on (the default).
    pub enabled: bool,
    /// The detected game; always `None` while disabled.
    pub activity: Option<Activity>,
    /// Games added on this device, matched before the built-in table.
    #[serde(default)]
    pub games: Vec<CustomGame>,
    /// Built-in game names never shown as an activity on this device.
    #[serde(default)]
    pub hidden: Vec<String>,
}

/// A game added on this device: its display name and normalized process name
/// (see [`process_key`]). Stored only in the device's `activity.json`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomGame {
    pub name: String,
    pub process: String,
}

/// The device-local `activity.json`. Older versions stored a bare boolean.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Preferences {
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default)]
    pub games: Vec<CustomGame>,
    #[serde(default)]
    pub hidden: Vec<String>,
}

fn enabled_by_default() -> bool {
    true
}

impl Default for Preferences {
    fn default() -> Self {
        Self {
            enabled: true,
            games: Vec::new(),
            hidden: Vec::new(),
        }
    }
}

impl Preferences {
    /// Missing or damaged preferences keep detection on with no added games.
    pub fn parse(data: &[u8]) -> Self {
        if let Ok(enabled) = serde_json::from_slice::<bool>(data) {
            return Self {
                enabled,
                ..Self::default()
            };
        }
        serde_json::from_slice::<Self>(data)
            .ok()
            .and_then(|p| {
                let (games, hidden) = validate(p.games, p.hidden).ok()?;
                Some(Self {
                    enabled: p.enabled,
                    games,
                    hidden,
                })
            })
            .unwrap_or_default()
    }
}

/// Most added games one device keeps.
pub const MAX_CUSTOM_GAMES: usize = 50;
/// Longest accepted process name, in characters.
pub const MAX_PROCESS_CHARS: usize = 128;

/// Every built-in display name, including the League match modes.
pub fn builtin_names() -> impl Iterator<Item = &'static str> {
    GAMES
        .iter()
        .map(|(name, _)| *name)
        .chain([TEAMFIGHT_TACTICS])
}

/// Normalizes and checks added and hidden games: names follow the activity
/// rules, process names are normalized, non-empty and unique, and only
/// built-in games can be hidden.
pub fn validate(
    games: Vec<CustomGame>,
    hidden: Vec<String>,
) -> Result<(Vec<CustomGame>, Vec<String>), &'static str> {
    if games.len() > MAX_CUSTOM_GAMES {
        return Err("You can add up to 50 games");
    }
    let mut keys = std::collections::HashSet::new();
    let mut valid = Vec::with_capacity(games.len());
    for game in games {
        let name = game.name.trim().to_owned();
        if Activity::playing(name.clone()).validate().is_err() {
            return Err("Game names must be 1 to 64 characters on one line");
        }
        let process = process_key(&game.process);
        if process.is_empty()
            || process.chars().count() > MAX_PROCESS_CHARS
            || process.chars().any(char::is_control)
        {
            return Err("Enter the game's program name, for example game.exe");
        }
        if !keys.insert(process.clone()) {
            return Err("That program is already in your games");
        }
        valid.push(CustomGame { name, process });
    }
    let mut names = Vec::new();
    for name in hidden {
        if !builtin_names().any(|builtin| builtin == name) {
            return Err("Only games Thiscord recognizes can be hidden");
        }
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok((valid, names))
}

/// A readable default name for a program ("SonsOfTheForest" → "Sons Of The
/// Forest", "hollow_knight.x86_64" → "Hollow Knight"). The user can edit it.
pub fn suggested_name(program: &str) -> String {
    let stem = program.rsplit(['/', '\\']).next().unwrap_or(program).trim();
    let mut stem = stem.to_owned();
    loop {
        let lower = stem.to_ascii_lowercase();
        let Some(suffix) = [
            ".exe",
            ".x86_64",
            ".x86",
            ".bin",
            "-win64-shipping",
            "-shipping",
            "-win64",
            "_x64",
            "_dx12",
            "_dx11",
            "_vulkan",
            "-x64",
        ]
        .into_iter()
        .find(|suffix| lower.ends_with(suffix) && lower.len() > suffix.len()) else {
            break;
        };
        stem.truncate(stem.len() - suffix.len());
    }
    let mut words = String::new();
    let mut previous: Option<char> = None;
    for c in stem.chars() {
        if matches!(c, '_' | '-' | '.' | ' ') {
            words.push(' ');
        } else {
            if previous.is_some_and(|p| p.is_lowercase() && c.is_uppercase()) {
                words.push(' ');
            }
            words.push(c);
        }
        previous = Some(c);
    }
    let name = words
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            chars
                .next()
                .map(|first| first.to_uppercase().chain(chars).collect::<String>())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>()
        .join(" ");
    name.chars()
        .take(MAX_ACTIVITY_CHARS)
        .collect::<String>()
        .trim()
        .to_owned()
}

/// A running program offered when adding a game.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningProgram {
    /// Normalized process name, as stored in [`CustomGame::process`].
    pub process: String,
    /// Suggested display name.
    pub name: String,
}

/// Running programs for the picker: one entry per process name, sorted by
/// suggested name, without `exclude` (this app) and system helpers.
pub fn running_programs<S: AsRef<str>>(
    processes: impl IntoIterator<Item = S>,
    exclude: &str,
) -> Vec<RunningProgram> {
    const HELPERS: &[&str] = &[
        "applicationframehost",
        "explorer",
        "lockapp",
        "msedgewebview2",
        "searchhost",
        "shellexperiencehost",
        "startmenuexperiencehost",
        "systemsettings",
        "textinputhost",
        "webkitwebprocess",
        "webkitnetworkprocess",
    ];
    let mut seen = std::collections::HashSet::new();
    let mut programs: Vec<RunningProgram> = processes
        .into_iter()
        .filter_map(|raw| {
            let raw = raw.as_ref();
            let process = process_key(raw);
            (!process.is_empty()
                && process != exclude
                && process.chars().count() <= MAX_PROCESS_CHARS
                && !process.chars().any(char::is_control)
                && !HELPERS.contains(&process.as_str())
                && seen.insert(process.clone()))
            .then(|| RunningProgram {
                name: suggested_name(raw),
                process,
            })
        })
        .filter(|program| !program.name.is_empty())
        .collect();
    programs.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.process.cmp(&b.process))
    });
    programs
}

pub const LEAGUE_OF_LEGENDS: &str = "League of Legends";
pub const TEAMFIGHT_TACTICS: &str = "Teamfight Tactics";

/// In-match League of Legends/Teamfight Tactics processes. Both modes share
/// this executable; the mode comes from the game's local live client API.
const LEAGUE_MATCH: &[&str] = &["league of legends", "leagueoflegends"];

/// Display name and normalized process names (see [`process_key`]).
pub const GAMES: &[(&str, &[&str])] = &[
    (LEAGUE_OF_LEGENDS, &["leagueclient", "leagueclientux"]),
    ("VALORANT", &["valorant-win64-shipping"]),
    ("Factorio", &["factorio"]),
    ("The Forest", &["theforest"]),
    ("Sons Of The Forest", &["sonsoftheforest"]),
    ("Counter-Strike 2", &["cs2"]),
    ("Dota 2", &["dota2"]),
    ("Minecraft", &["minecraft.windows"]),
    ("Fortnite", &["fortniteclient-win64-shipping"]),
    ("Apex Legends", &["r5apex", "r5apex_dx12"]),
    ("Rocket League", &["rocketleague"]),
    ("Overwatch 2", &["overwatch"]),
    ("Grand Theft Auto V", &["gta5", "gta5_enhanced"]),
    ("ELDEN RING", &["eldenring"]),
    ("Baldur's Gate 3", &["bg3", "bg3_dx11"]),
    ("Terraria", &["terraria", "terraria.bin.x86_64"]),
    (
        "Stardew Valley",
        &["stardew valley", "stardewvalley", "stardewmoddingapi"],
    ),
    ("Valheim", &["valheim", "valheim.x86_64"]),
    ("Rust", &["rustclient"]),
    ("Among Us", &["among us"]),
    ("Lethal Company", &["lethal company"]),
    ("Phasmophobia", &["phasmophobia"]),
    (
        "Satisfactory",
        &[
            "factorygame-win64-shipping",
            "factorygamesteam-win64-shipping",
            "factorygameegs-win64-shipping",
        ],
    ),
    ("Hearthstone", &["hearthstone"]),
    ("World of Warcraft", &["wow", "wowclassic"]),
    (
        "Tom Clancy's Rainbow Six Siege",
        &["rainbowsix", "rainbowsix_vulkan"],
    ),
    ("PUBG: BATTLEGROUNDS", &["tslgame"]),
    ("Hollow Knight", &["hollow_knight", "hollow_knight.x86_64"]),
    ("Palworld", &["palworld-win64-shipping"]),
    ("HELLDIVERS 2", &["helldivers2"]),
    ("Cyberpunk 2077", &["cyberpunk2077"]),
    ("The Witcher 3: Wild Hunt", &["witcher3"]),
    (
        "Don't Starve Together",
        &["dontstarve_steam_x64", "dontstarve_steam"],
    ),
    ("RimWorld", &["rimworldwin64", "rimworldlinux"]),
    ("Deep Rock Galactic", &["fsd-win64-shipping"]),
    ("Dead by Daylight", &["deadbydaylight-win64-shipping"]),
    (
        "Path of Exile",
        &["pathofexile", "pathofexile_x64", "pathofexilesteam"],
    ),
    ("Genshin Impact", &["genshinimpact", "yuanshen"]),
    ("osu!", &["osu!"]),
    ("Euro Truck Simulator 2", &["eurotrucks2"]),
    ("Sea of Thieves", &["sotgame"]),
    ("Warframe", &["warframe.x64"]),
    ("Destiny 2", &["destiny2"]),
    ("Monster Hunter: World", &["monsterhunterworld"]),
    ("Monster Hunter Wilds", &["monsterhunterwilds"]),
    ("Risk of Rain 2", &["risk of rain 2"]),
    ("Subnautica", &["subnautica"]),
    ("Oxygen Not Included", &["oxygennotincluded"]),
    ("No Man's Sky", &["nms"]),
    ("Diablo IV", &["diablo iv"]),
    ("Celeste", &["celeste"]),
];

/// Lower-case file name without directories (either separator, so Wine/Proton
/// paths on Linux match) or a trailing `.exe`.
pub fn process_key(raw: &str) -> String {
    let name = raw.rsplit(['/', '\\']).next().unwrap_or(raw).trim();
    let name = name.to_lowercase();
    match name.strip_suffix(".exe") {
        Some(stem) => stem.to_owned(),
        None => name,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detected {
    Game(&'static str),
    /// A League of Legends or Teamfight Tactics match; see [`league_mode`].
    LeagueMatch,
    /// An added game, by its index in the device's list.
    Custom(usize),
}

/// The detected game among running processes. A League match wins over its
/// client; otherwise the earliest table entry wins, so results are stable.
pub fn detect<S: AsRef<str>>(processes: impl IntoIterator<Item = S>) -> Option<Detected> {
    detect_with(processes, &[], &[])
}

/// Like [`detect`], but added games come first (in list order) and hidden
/// built-in games are skipped. Callers also drop a hidden League match mode.
pub fn detect_with<S: AsRef<str>>(
    processes: impl IntoIterator<Item = S>,
    games: &[CustomGame],
    hidden: &[String],
) -> Option<Detected> {
    let keys: std::collections::HashSet<String> = processes
        .into_iter()
        .map(|p| process_key(p.as_ref()))
        .collect();
    if let Some(index) = games.iter().position(|game| keys.contains(&game.process)) {
        return Some(Detected::Custom(index));
    }
    let shown = |name: &str| !hidden.iter().any(|h| h == name);
    if (shown(LEAGUE_OF_LEGENDS) || shown(TEAMFIGHT_TACTICS))
        && LEAGUE_MATCH.iter().any(|key| keys.contains(*key))
    {
        return Some(Detected::LeagueMatch);
    }
    GAMES
        .iter()
        .filter(|(name, _)| shown(name))
        .find(|(_, names)| names.iter().any(|key| keys.contains(*key)))
        .map(|(name, _)| Detected::Game(name))
}

/// The game name from the League live client API's `gamestats` response
/// (`{"gameMode":"TFT",...}`); `None` while the match has not loaded.
pub fn league_mode(gamestats: &[u8]) -> Option<&'static str> {
    #[derive(Deserialize)]
    struct Stats {
        #[serde(rename = "gameMode")]
        game_mode: String,
    }
    let stats: Stats = serde_json::from_slice(gamestats).ok()?;
    Some(if stats.game_mode.eq_ignore_ascii_case("TFT") {
        TEAMFIGHT_TACTICS
    } else {
        LEAGUE_OF_LEGENDS
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_names_normalize_across_platforms() {
        assert_eq!(process_key("Factorio.exe"), "factorio");
        assert_eq!(process_key("/opt/factorio/bin/x64/factorio"), "factorio");
        assert_eq!(
            process_key(r"Z:\home\me\.steam\steamapps\common\The Forest\TheForest.exe"),
            "theforest"
        );
        assert_eq!(
            process_key("/Applications/League of Legends.app/Contents/MacOS/League of Legends"),
            "league of legends"
        );
        assert_eq!(process_key("Terraria.bin.x86_64"), "terraria.bin.x86_64");
        assert_eq!(process_key(""), "");
    }

    #[test]
    fn detection_prefers_matches_and_is_stable() {
        assert_eq!(detect(["explorer.exe", "svchost.exe"]), None);
        assert_eq!(detect::<&str>([]), None);
        assert_eq!(
            detect(["steam.exe", "factorio.exe"]),
            Some(Detected::Game("Factorio"))
        );
        assert_eq!(
            detect(["LeagueClientUx.exe", "RiotClientServices.exe"]),
            Some(Detected::Game(LEAGUE_OF_LEGENDS))
        );
        assert_eq!(
            detect(["LeagueClientUx.exe", "League of Legends.exe"]),
            Some(Detected::LeagueMatch)
        );
        // Table order decides between unrelated games, not process order.
        assert_eq!(
            detect(["SonsOfTheForest.exe", "factorio"]),
            Some(Detected::Game("Factorio"))
        );
        // Similar names are not prefixes.
        assert_eq!(detect(["factorio-server-helper", "cs2_launcher"]), None);
    }

    #[test]
    fn table_is_normalized_unique_and_valid() {
        let mut seen = std::collections::HashSet::new();
        for (name, keys) in GAMES {
            assert!(Activity::playing(*name).validate().is_ok(), "{name}");
            assert!(!keys.is_empty(), "{name}");
            for key in *keys {
                assert_eq!(process_key(key), *key, "{name}: {key}");
                assert!(seen.insert(*key), "duplicate {key}");
            }
        }
        for key in LEAGUE_MATCH {
            assert_eq!(process_key(key), *key);
            assert!(seen.insert(*key));
        }
    }

    fn game(name: &str, process: &str) -> CustomGame {
        CustomGame {
            name: name.into(),
            process: process.into(),
        }
    }

    #[test]
    fn added_games_win_and_hidden_games_are_skipped() {
        let games = [
            game("My Mod", "factorio"),
            game("Hollow Knight", "hollow_knight"),
        ];
        assert_eq!(
            detect_with(["Factorio.exe"], &games, &[]),
            Some(Detected::Custom(0))
        );
        assert_eq!(
            detect_with(["/games/hollow_knight"], &games, &[]),
            Some(Detected::Custom(1))
        );
        let hidden = ["Factorio".to_owned()];
        assert_eq!(
            detect_with(["factorio", "cs2"], &[], &hidden),
            Some(Detected::Game("Counter-Strike 2"))
        );
        assert_eq!(detect_with(["factorio"], &[], &hidden), None);
        // A hidden built-in game can still be added under another name.
        assert_eq!(
            detect_with(["factorio"], &games[..1], &hidden),
            Some(Detected::Custom(0))
        );
        let both = [LEAGUE_OF_LEGENDS.to_owned(), TEAMFIGHT_TACTICS.to_owned()];
        assert_eq!(detect_with(["League of Legends.exe"], &[], &both), None);
        assert_eq!(
            detect_with(["League of Legends.exe"], &[], &both[1..]),
            Some(Detected::LeagueMatch)
        );
    }

    #[test]
    fn preferences_validate_and_read_older_files() {
        assert_eq!(
            Preferences::parse(b"false"),
            Preferences {
                enabled: false,
                ..Default::default()
            }
        );
        assert_eq!(Preferences::parse(b"true"), Preferences::default());
        assert_eq!(Preferences::parse(b"not json"), Preferences::default());
        let saved = Preferences {
            enabled: false,
            games: vec![game("Hollow Knight", "hollow_knight")],
            hidden: vec!["Factorio".into()],
        };
        assert_eq!(
            Preferences::parse(&serde_json::to_vec(&saved).unwrap()),
            saved
        );
        let (games, hidden) = validate(
            vec![game("  Spaced  ", r"C:\Games\My Game.EXE")],
            vec![
                "Factorio".into(),
                "Factorio".into(),
                TEAMFIGHT_TACTICS.into(),
            ],
        )
        .unwrap();
        assert_eq!(games, [game("Spaced", "my game")]);
        assert_eq!(hidden, ["Factorio", TEAMFIGHT_TACTICS]);
        for (games, hidden) in [
            (vec![game("", "x")], vec![]),
            (vec![game("x".repeat(65).as_str(), "x")], vec![]),
            (vec![game("Two\nlines", "x")], vec![]),
            (vec![game("A", " ")], vec![]),
            (vec![game("A", "x.exe"), game("B", "X")], vec![]),
            (vec![], vec!["Not a built-in game".to_owned()]),
            (
                (0..=MAX_CUSTOM_GAMES)
                    .map(|i| game("A", &i.to_string()))
                    .collect(),
                vec![],
            ),
        ] {
            assert!(validate(games, hidden).is_err());
        }
        // Damaged lists are dropped rather than half-applied.
        assert_eq!(
            Preferences::parse(br#"{"enabled":false,"games":[{"name":"","process":"x"}]}"#),
            Preferences::default()
        );
    }

    #[test]
    fn running_programs_get_readable_unique_suggestions() {
        assert_eq!(suggested_name("SonsOfTheForest.exe"), "Sons Of The Forest");
        assert_eq!(
            suggested_name("/opt/hk/hollow_knight.x86_64"),
            "Hollow Knight"
        );
        assert_eq!(
            suggested_name("FactoryGame-Win64-Shipping.exe"),
            "Factory Game"
        );
        assert_eq!(suggested_name(r"Z:\games\osu!.exe"), "Osu!");
        assert_eq!(suggested_name(".exe"), "Exe");
        let programs = running_programs(
            [
                "explorer.exe",
                "Thiscord.exe",
                "hollow_knight.exe",
                "/usr/games/hollow_knight",
                "Zeta.exe",
                "alpha",
            ],
            "thiscord",
        );
        assert_eq!(
            programs,
            [
                RunningProgram {
                    process: "alpha".into(),
                    name: "Alpha".into()
                },
                RunningProgram {
                    process: "hollow_knight".into(),
                    name: "Hollow Knight".into()
                },
                RunningProgram {
                    process: "zeta".into(),
                    name: "Zeta".into()
                },
            ]
        );
    }

    #[test]
    fn league_modes_come_from_live_client_stats() {
        assert_eq!(
            league_mode(br#"{"gameMode":"TFT","gameTime":12.5}"#),
            Some(TEAMFIGHT_TACTICS)
        );
        assert_eq!(
            league_mode(br#"{"gameMode":"CLASSIC"}"#),
            Some(LEAGUE_OF_LEGENDS)
        );
        assert_eq!(
            league_mode(br#"{"gameMode":"ARAM"}"#),
            Some(LEAGUE_OF_LEGENDS)
        );
        assert_eq!(league_mode(b"not json"), None);
        assert_eq!(league_mode(br#"{"errorCode":"RESOURCE_NOT_FOUND"}"#), None);
    }
}
