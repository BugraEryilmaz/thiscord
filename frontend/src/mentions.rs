//! Caret-aware mention completion. Browser selections use UTF-16 code units.
use thiscord_shared::chat::{MAX_MENTION_QUERY_CHARS, MAX_MESSAGE_CHARS};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Query {
    pub start: usize,
    pub end: usize,
    pub text: String,
}

fn byte_offset(text: &str, utf16: u32) -> Option<usize> {
    let mut position = 0;
    for (byte, ch) in text.char_indices() {
        if position == utf16 {
            return Some(byte);
        }
        position += ch.len_utf16() as u32;
    }
    (position == utf16).then_some(text.len())
}

pub fn query(text: &str, start: u32, end: u32) -> Option<Query> {
    if start != end {
        return None;
    }
    let caret = byte_offset(text, start)?;
    let at = text[..caret].rfind('@')?;
    // Match the server's whitespace-delimited @username syntax, not emails/URLs.
    if at > 0 && !text[..at].chars().next_back()?.is_whitespace() {
        return None;
    }
    let search = &text[at + 1..caret];
    if search.chars().count() > MAX_MENTION_QUERY_CHARS
        || search.chars().any(char::is_control)
        || search.starts_with(char::is_whitespace)
    {
        return None;
    }
    // Replace the remainder of the current word when completing in its middle.
    let suffix = text[caret..]
        .chars()
        .take_while(|ch| ch.is_alphanumeric() || *ch == '_')
        .map(char::len_utf8)
        .sum::<usize>();
    Some(Query {
        start: at,
        end: caret + suffix,
        text: search.to_owned(),
    })
}

pub fn insert(text: &str, query: &Query, username: &str) -> Option<(String, u32)> {
    let before = text.get(..query.start)?;
    let after = text.get(query.end..)?;
    let prefix = format!("{before}@{username}");
    let space = if after.is_empty() { " " } else { "" };
    let result = format!("{prefix}{space}{after}");
    if result.chars().count() > MAX_MESSAGE_CHARS {
        return None;
    }
    let caret =
        (prefix.encode_utf16().count() + space.len() + usize::from(after.starts_with(' '))) as u32;
    Some((result, caret))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn searches_profile_names_with_spaces_unicode_and_handles() {
        for text in ["@", "Hi @Ada Lov", "🎮 @Büğra", "@user_12", "line\n@name"] {
            let caret = text.encode_utf16().count() as u32;
            assert!(query(text, caret, caret).is_some(), "{text}");
        }
        for text in ["a@b", "https://a/@b", "@a\nb", "@ name"] {
            let caret = text.encode_utf16().count() as u32;
            assert!(query(text, caret, caret).is_none(), "{text}");
        }
        assert!(query("@abc", 1, 3).is_none());
        assert!(query("🎮 @a", 1, 1).is_none());
        let long = format!("@{}", "a".repeat(65));
        assert!(query(&long, 66, 66).is_none());
    }
    #[test]
    fn insertion_preserves_surroundings_and_utf16_caret() {
        let text = "🎮 @Ada Lov, hello @other";
        let caret = "🎮 @Ada Lov".encode_utf16().count() as u32;
        let q = query(text, caret, caret).unwrap();
        let (result, caret) = insert(text, &q, "ada_2").unwrap();
        assert_eq!(result, "🎮 @ada_2, hello @other");
        assert_eq!(caret, "🎮 @ada_2".encode_utf16().count() as u32);
        let q = query("Hi @albert next", 6, 6).unwrap();
        assert_eq!(
            insert("Hi @albert next", &q, "alice").unwrap(),
            ("Hi @alice next".into(), 10)
        );
    }
    #[test]
    fn insertion_respects_message_limit() {
        let text = format!("{} @a", "x".repeat(3997));
        let q = query(&text, 4000, 4000).unwrap();
        assert!(insert(&text, &q, "long_name").is_none());
    }
}
