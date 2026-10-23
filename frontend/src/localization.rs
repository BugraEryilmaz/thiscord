//! Portable presentation-only translations. Never translate protocol values,
//! message bodies, names, paths, or other user-provided content.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Language {
    #[default]
    English,
    Turkish,
}

impl Language {
    pub fn code(self) -> &'static str {
        match self {
            Self::English => "en",
            Self::Turkish => "tr",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        match tag.split(['-', '_']).next()?.to_ascii_lowercase().as_str() {
            "en" => Some(Self::English),
            "tr" => Some(Self::Turkish),
            _ => None,
        }
    }

    /// A valid saved choice wins. Otherwise choose the first supported system
    /// language, with English as the fallback for an unsupported/empty list.
    pub fn resolve(saved: Option<&str>, system: &[String]) -> Self {
        saved
            .and_then(Self::from_tag)
            .or_else(|| system.iter().find_map(|tag| Self::from_tag(tag)))
            .unwrap_or_default()
    }
}

include!(concat!(env!("OUT_DIR"), "/translations.rs"));

/// Preserve safe server details in English. For Turkish, use a known translation
/// or the stable wire code; never expose an internal error or drop the request ID.
pub fn api_error(language: Language, error: &thiscord_shared::ApiError) -> String {
    use thiscord_shared::ErrorCode;
    let translated = translate(language, &error.message);
    let message = if language == Language::English || translated != error.message {
        translated
    } else {
        translate(
            language,
            match error.code {
                ErrorCode::BadRequest | ErrorCode::ValidationFailed => {
                    "Check the information you entered."
                }
                ErrorCode::Unauthorized => "Please sign in again.",
                ErrorCode::Forbidden => "You do not have permission to do that.",
                ErrorCode::Conflict => "Something changed. Refresh and try again.",
                ErrorCode::LimitExceeded => {
                    "A role limit has been reached. Review your server limits."
                }
                ErrorCode::RateLimited => "Too many requests. Please wait and try again.",
                ErrorCode::NotFound => "The requested item was not found.",
                ErrorCode::MethodNotAllowed => "This action is not supported.",
                ErrorCode::ServiceUnavailable => {
                    "Thiscord is temporarily unavailable. Try again shortly."
                }
                ErrorCode::InternalError => "Something went wrong. Please try again.",
            },
        )
    };
    localized_format!(language, "{} (request {})", message, error.request_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rich_chat_and_search_controls_have_turkish_translations() {
        for source in [
            "Search messages",
            "Close search",
            "Words",
            "Person (@username)",
            "All accessible channels",
            "From (UTC)",
            "Before (UTC)",
            "Searching…",
            "Search complete",
            "No messages found",
            "Search timed out",
            "Invalid date",
            "View original and context",
            "Open channel",
            "Original message and nearby history",
            "Reply",
            "Reply selected",
            "Reply to {}: {}",
            "Cancel reply",
            "Attachment message",
            "Original message unavailable",
            "React {}",
            "Pin",
            "Unpin",
            "Pinned messages",
            "Pinned messages (up to 50)",
            "No pinned messages",
            "Invalid search request",
            "Search needs 1 to 200 characters",
            "Use an exact username for the person filter",
            "Search start must be before its end",
            "Select a conversation to narrow this search",
            "Unsupported reaction",
            "A channel can have at most 50 pinned messages",
        ] {
            assert_ne!(
                translate(Language::Turkish, source),
                source,
                "missing Turkish: {source}"
            );
            assert_eq!(translate(Language::English, source), source);
        }
    }

    #[test]
    fn preference_and_system_language_negotiation() {
        let system = ["de-CH".into(), "tr-TR".into(), "en-US".into()];
        assert_eq!(Language::resolve(Some("en"), &system), Language::English);
        assert_eq!(Language::resolve(None, &system), Language::Turkish);
        assert_eq!(
            Language::resolve(Some("invalid"), &system),
            Language::Turkish
        );
        assert_eq!(Language::resolve(None, &["ja".into()]), Language::English);
        assert_eq!(Language::resolve(None, &[]), Language::English);
        assert_eq!(Language::from_tag("TR_tr"), Some(Language::Turkish));
    }

    #[test]
    fn fallback_and_interpolation_do_not_translate_user_content() {
        assert_eq!(translate(Language::English, "Settings"), "Settings");
        assert_eq!(translate(Language::Turkish, "Settings"), "Ayarlar");
        assert_eq!(
            translate(Language::Turkish, "A future message"),
            "A future message"
        );
        let name = "Settings <script> {name} İpek";
        assert_eq!(
            localized_format!(Language::Turkish, "{} typing…", name),
            "Settings <script> {name} İpek yazıyor…"
        );
    }

    fn placeholders(text: &str) -> Vec<&str> {
        let mut result: Vec<_> = text
            .split('{')
            .skip(1)
            .map(|rest| rest.split_once('}').expect("closed placeholder").0)
            .collect();
        result.sort_unstable();
        result
    }

    #[test]
    fn catalog_preserves_all_format_arguments() {
        for &(en, tr) in CATALOG {
            assert!(!tr.trim().is_empty(), "empty translation: {en}");
            assert_eq!(placeholders(en), placeholders(tr), "format arguments: {en}");
        }
    }

    #[test]
    fn translated_wire_errors_keep_request_ids() {
        let error = thiscord_shared::ApiError {
            code: thiscord_shared::ErrorCode::Forbidden,
            message: "Unrecognized safe server detail".into(),
            request_id: "00000000-0000-0000-0000-000000000042".parse().unwrap(),
            fields: vec![],
        };
        let text = api_error(Language::Turkish, &error);
        assert!(text.contains("Bu işlemi yapma iznin yok."));
        assert!(text.contains(&error.request_id.to_string()));
        assert!(api_error(Language::English, &error).starts_with(&error.message));
    }

    #[test]
    fn ui_translation_keys_and_shared_labels_have_catalog_entries() {
        use std::{fs, path::Path};
        fn check_file(path: &Path) {
            let source = fs::read_to_string(path).unwrap();
            for rest in source.split("i18n::t(").skip(1) {
                if let Some('"') = rest.trim_start().chars().next() {
                    let key = serde_json::Deserializer::from_str(rest.trim_start())
                        .into_iter::<String>()
                        .next()
                        .unwrap()
                        .unwrap();
                    assert!(
                        CATALOG.iter().any(|(en, _)| *en == key),
                        "missing translation in {}: {key}",
                        path.display()
                    );
                }
            }
        }
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        for file in ["app.rs", "i18n.rs", "overlay.rs", "notification_banner.rs"] {
            check_file(&root.join(file));
        }
        for file in fs::read_dir(root.join("app")).unwrap() {
            let path = file.unwrap().path();
            if path.extension().is_some_and(|ext| ext == "rs") {
                check_file(&path);
            }
        }
        use thiscord_shared::{
            notifications::{Kind, Snooze},
            voice_bots::{ConnectionProfile, MicrophoneLoop},
        };
        for label in Kind::ALL
            .map(Kind::label)
            .into_iter()
            .chain(Snooze::OPTIONS.map(|(_, label)| label))
            .chain(ConnectionProfile::ALL.map(ConnectionProfile::label))
            .chain(MicrophoneLoop::ALL.map(MicrophoneLoop::label))
            .chain(Kind::ALL.map(crate::notification_sound::description))
        {
            assert!(
                CATALOG.iter().any(|(en, _)| *en == label),
                "missing shared label: {label}"
            );
        }
    }
}
