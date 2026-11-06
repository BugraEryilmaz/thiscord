//! Release changelogs shown on update screens.
//!
//! `changelog/<version>.md` (English) and its translations such as
//! `changelog/<version>.tr.md` are published in the GitHub release body and in
//! the `notes` of the update manifest (`docs/releases.md`). The text comes from
//! the network, so this parses a small Markdown subset into plain spans; nothing
//! is ever rendered as HTML.

/// Notes beyond this many bytes are ignored; release validation keeps them far smaller.
pub const MAX_NOTES_BYTES: usize = 32 * 1024;
const MAX_ENTRIES: usize = 80;
/// Manifest notes hold English first, then each translation after this marker,
/// e.g. `<!-- lang: tr -->`. GitHub hides the comment in rendered Markdown.
pub const LANGUAGE_MARKER: &str = "<!-- lang:";

/// The part of the notes written in `language` (a code like `tr`), or the
/// English part when that translation is missing.
pub fn localized<'a>(notes: &'a str, language: &str) -> &'a str {
    let mut parts = notes.split(LANGUAGE_MARKER);
    let english = parts.next().unwrap_or_default();
    parts
        .filter_map(|part| part.split_once("-->"))
        .find(|(code, _)| code.trim() == language)
        .map_or(english, |(_, text)| text)
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changelog {
    pub highlights: Vec<Entry>,
    pub sections: Vec<Section>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    New,
    Improved,
    Fixed,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    pub kind: Kind,
    /// The heading as written; empty for text before the first heading.
    pub title: String,
    pub entries: Vec<Entry>,
}

/// One bullet. A leading `**Title**` becomes the title, the rest the description.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    pub title: Option<String>,
    pub text: Vec<Span>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Span {
    Text(String),
    Strong(String),
    Code(String),
}

impl Changelog {
    pub fn is_empty(&self) -> bool {
        self.highlights.is_empty() && self.sections.is_empty()
    }

    /// Number of entries behind "Show all changes".
    pub fn detail_count(&self) -> usize {
        self.sections.iter().map(|s| s.entries.len()).sum()
    }

    pub fn parse(notes: &str) -> Self {
        let mut end = notes.len().min(MAX_NOTES_BYTES);
        while !notes.is_char_boundary(end) {
            end -= 1;
        }
        let mut highlights = Vec::new();
        let mut sections: Vec<Section> = Vec::new();
        let mut in_highlights = false;
        let mut current: Option<Section> = None;
        let mut entry: Option<String> = None;
        let mut total = 0;
        let mut flush = |entry: &mut Option<String>,
                         in_highlights: bool,
                         current: &mut Option<Section>,
                         highlights: &mut Vec<Entry>| {
            let Some(raw) = entry.take() else { return };
            if total >= MAX_ENTRIES || raw.trim().is_empty() {
                return;
            }
            total += 1;
            let parsed = Entry::parse(raw.trim());
            if in_highlights {
                highlights.push(parsed);
            } else {
                current
                    .get_or_insert_with(|| Section {
                        kind: Kind::Other,
                        title: String::new(),
                        entries: Vec::new(),
                    })
                    .entries
                    .push(parsed);
            }
        };
        for line in notes[..end].lines() {
            let trimmed = line.trim();
            if let Some(heading) = trimmed.strip_prefix("## ") {
                flush(&mut entry, in_highlights, &mut current, &mut highlights);
                sections.extend(current.take().filter(|s| !s.entries.is_empty()));
                let title = heading.trim().trim_end_matches('#').trim().to_owned();
                in_highlights = title.eq_ignore_ascii_case("highlights");
                if !in_highlights {
                    current = Some(Section {
                        kind: Kind::from_title(&title),
                        title,
                        entries: Vec::new(),
                    });
                }
            } else if trimmed.starts_with('#') || trimmed.is_empty() {
                // A document title, or a blank line ending a paragraph entry.
                flush(&mut entry, in_highlights, &mut current, &mut highlights);
            } else if let Some(item) = bullet(line) {
                flush(&mut entry, in_highlights, &mut current, &mut highlights);
                entry = Some(item.to_owned());
            } else if let Some(text) = &mut entry {
                text.push(' ');
                text.push_str(trimmed);
            } else {
                entry = Some(trimmed.to_owned());
            }
        }
        flush(&mut entry, in_highlights, &mut current, &mut highlights);
        sections.extend(current.filter(|s| !s.entries.is_empty()));
        Self {
            highlights,
            sections,
        }
    }
}

/// A top-level `-`/`*` bullet. Nested bullets continue their parent entry.
fn bullet(line: &str) -> Option<&str> {
    let rest = line
        .strip_prefix("- ")
        .or_else(|| line.strip_prefix("* "))?;
    Some(rest.trim())
}

impl Kind {
    fn from_title(title: &str) -> Self {
        match title.to_ascii_lowercase().as_str() {
            "new" => Self::New,
            "improved" => Self::Improved,
            "fixed" => Self::Fixed,
            _ => Self::Other,
        }
    }
}

impl Entry {
    fn parse(raw: &str) -> Self {
        let mut spans = spans(raw);
        let title = match spans.first() {
            Some(Span::Strong(title)) => {
                let title = title.trim_end_matches(['.', ':']).trim().to_owned();
                spans.remove(0);
                if let Some(Span::Text(text)) = spans.first_mut() {
                    let rest = text
                        .trim_start()
                        .trim_start_matches(['—', '–', '-', ':', '.'])
                        .trim_start();
                    *text = rest.to_owned();
                    if text.is_empty() {
                        spans.remove(0);
                    }
                }
                Some(title)
            }
            _ => None,
        };
        Self { title, text: spans }
    }
}

/// `**strong**` and `` `code` `` spans; links keep their label. Unmatched
/// markers stay literal.
fn spans(raw: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut text = String::new();
    let mut rest = raw;
    let push = |spans: &mut Vec<Span>, text: &mut String| {
        if !text.is_empty() {
            spans.push(Span::Text(std::mem::take(text)));
        }
    };
    while let Some(c) = rest.chars().next() {
        if let Some(after) = rest.strip_prefix("**")
            && let Some(close) = after.find("**")
            && close > 0
        {
            push(&mut spans, &mut text);
            spans.push(Span::Strong(after[..close].to_owned()));
            rest = &after[close + 2..];
        } else if let Some(after) = rest.strip_prefix('`')
            && let Some(close) = after.find('`')
            && close > 0
        {
            push(&mut spans, &mut text);
            spans.push(Span::Code(after[..close].to_owned()));
            rest = &after[close + 1..];
        } else if let Some(after) = rest.strip_prefix('[')
            && let Some(close) = after.find("](")
            && let Some(end) = after[close..].find(')')
        {
            text.push_str(&after[..close]);
            rest = &after[close + end + 1..];
        } else {
            text.push(c);
            rest = &rest[c.len_utf8()..];
        }
    }
    push(&mut spans, &mut text);
    spans
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(entry: &Entry) -> String {
        entry
            .text
            .iter()
            .map(|s| match s {
                Span::Text(t) | Span::Strong(t) | Span::Code(t) => t.as_str(),
            })
            .collect()
    }

    #[test]
    fn parses_highlights_and_grouped_details() {
        let log = Changelog::parse(
            "# Thiscord 1.2.3\n\n## Highlights\n\n\
             - **Changelogs.** Update screens show what changed.\n\
             - **Faster voice** — less CPU,\n  fewer drops.\n\n\
             ## New\n\n- Added `thiscord.x` setting and **bold** words.\n\
             ## Fixed\n* Scrolling no longer [jumps](https://example.com).\n\
             ## Under the hood ##\n- Refactors.\n",
        );
        assert_eq!(log.highlights.len(), 2);
        assert_eq!(log.highlights[0].title.as_deref(), Some("Changelogs"));
        assert_eq!(
            text(&log.highlights[0]),
            "Update screens show what changed."
        );
        assert_eq!(log.highlights[1].title.as_deref(), Some("Faster voice"));
        assert_eq!(text(&log.highlights[1]), "less CPU, fewer drops.");
        let kinds: Vec<_> = log
            .sections
            .iter()
            .map(|s| (s.kind, s.title.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                (Kind::New, "New"),
                (Kind::Fixed, "Fixed"),
                (Kind::Other, "Under the hood")
            ]
        );
        assert_eq!(
            log.sections[0].entries[0].text,
            [
                Span::Text("Added ".into()),
                Span::Code("thiscord.x".into()),
                Span::Text(" setting and ".into()),
                Span::Strong("bold".into()),
                Span::Text(" words.".into()),
            ]
        );
        assert_eq!(
            text(&log.sections[1].entries[0]),
            "Scrolling no longer jumps."
        );
        assert_eq!(log.detail_count(), 3);
    }

    #[test]
    fn unstructured_notes_become_details() {
        let log = Changelog::parse("Thiscord 0.1.59. Install when you are ready to restart.");
        assert_eq!(log.sections.len(), 1);
        assert_eq!(log.sections[0].kind, Kind::Other);
        assert!(log.highlights.is_empty());
        assert!(Changelog::parse("  \n\n").is_empty());
    }

    #[test]
    fn hostile_notes_stay_bounded_text() {
        let huge = "- <script>alert(1)</script> **x\n".repeat(10_000);
        let log = Changelog::parse(&huge);
        assert_eq!(log.detail_count(), MAX_ENTRIES);
        assert_eq!(
            log.sections[0].entries[0].text,
            [Span::Text("<script>alert(1)</script> **x".into())]
        );
        // Multi-byte text cut at the byte limit still parses.
        let log = Changelog::parse(&"ğ".repeat(MAX_NOTES_BYTES));
        assert_eq!(log.detail_count(), 1);
        assert_eq!(spans("****"), [Span::Text("****".into())]);
    }

    #[test]
    fn notes_select_the_translation_or_fall_back_to_english() {
        let notes = "## Highlights\n- **Hello.** World.\n\n<!-- lang: tr -->\n\n## Highlights\n- **Merhaba.** Dünya.\n";
        let title = |language| {
            Changelog::parse(localized(notes, language)).highlights[0]
                .title
                .clone()
        };
        assert_eq!(title("en").as_deref(), Some("Hello"));
        assert_eq!(title("tr").as_deref(), Some("Merhaba"));
        assert_eq!(title("de").as_deref(), Some("Hello"));
        assert_eq!(Changelog::parse(localized(notes, "en")).detail_count(), 0);
        assert_eq!(localized("only English", "tr"), "only English");
    }

    /// Every committed changelog is well formed and matches its English original,
    /// and the version being built has every translation, so a release PR cannot
    /// skip one (`docs/releases.md`).
    #[test]
    fn committed_changelogs_are_well_formed() {
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../changelog");
        for suffix in ["md", "tr.md"] {
            let current = directory.join(format!("{}.{suffix}", env!("CARGO_PKG_VERSION")));
            assert!(
                current.is_file(),
                "add {} before releasing this version",
                current.display()
            );
        }
        let shape = |log: &Changelog| {
            (
                log.highlights.len(),
                log.sections
                    .iter()
                    .map(|s| (s.kind, s.entries.len()))
                    .collect::<Vec<_>>(),
            )
        };
        for file in std::fs::read_dir(&directory).unwrap() {
            let path = file.unwrap().path();
            let notes = std::fs::read_to_string(&path).unwrap();
            let name = path.display();
            assert!(
                !notes.contains(LANGUAGE_MARKER),
                "{name} contains a language marker"
            );
            if let Some(version) = path.to_str().unwrap().strip_suffix(".tr.md") {
                let english = std::fs::read_to_string(format!("{version}.md"))
                    .unwrap_or_else(|_| panic!("{name} has no English original"));
                assert_eq!(
                    shape(&Changelog::parse(&notes)),
                    shape(&Changelog::parse(&english)),
                    "{name} must translate the same highlights and bullets as the English file"
                );
            }
            assert!(notes.len() <= 8 * 1024, "{name} exceeds 8 KiB");
            assert!(
                notes.contains("\n## Highlights\n") || notes.starts_with("## Highlights\n"),
                "{name} needs ## Highlights"
            );
            let log = Changelog::parse(&notes);
            assert!(
                (1..=4).contains(&log.highlights.len()),
                "{name} needs 1-4 highlights"
            );
            assert!(
                log.highlights
                    .iter()
                    .all(|h| h.title.is_some() && !h.text.is_empty()),
                "{name}: start each highlight with a **Title.** and a sentence"
            );
            for section in &log.sections {
                assert_ne!(
                    section.kind,
                    Kind::Other,
                    "{name}: use New, Improved or Fixed, not {:?}",
                    section.title
                );
            }
        }
    }
}
