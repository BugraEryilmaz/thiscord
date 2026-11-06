//! Platform-neutral game detection for the desktop "Playing …" status.
//!
//! The desktop app lists running process names locally and matches them against
//! this built-in table. Process names never leave the device; only the matched
//! display name is sent, and only while sharing is enabled (see docs/activity.md).
use serde::{Deserialize, Serialize};
use thiscord_shared::account::Activity;

/// Desktop event carrying an [`ActivityStatus`] whenever either field changes.
pub const ACTIVITY_EVENT: &str = "activity-status";

/// Device-local detection state shared between the native app and the UI.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityStatus {
    /// Detection and sharing are on (the default).
    pub enabled: bool,
    /// The detected game; always `None` while disabled.
    pub activity: Option<Activity>,
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
}

/// The detected game among running processes. A League match wins over its
/// client; otherwise the earliest table entry wins, so results are stable.
pub fn detect<S: AsRef<str>>(processes: impl IntoIterator<Item = S>) -> Option<Detected> {
    let keys: std::collections::HashSet<String> = processes
        .into_iter()
        .map(|p| process_key(p.as_ref()))
        .collect();
    if LEAGUE_MATCH.iter().any(|key| keys.contains(*key)) {
        return Some(Detected::LeagueMatch);
    }
    GAMES
        .iter()
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
