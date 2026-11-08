//! Bounded Markdown AST; the UI constructs DOM nodes and never accepts raw HTML.
//! `||spoilers||` are paired within one inline container. Only literal `||` in
//! the source delimits them (never escapes or entities), so
//! `thiscord_shared::chat::mask_spoilers` hides at least everything hidden here.
use pulldown_cmark::{Event, Options, Parser, Tag};
#[derive(Clone, Debug, PartialEq)]
pub enum Kind {
    Plain,
    Paragraph,
    Strong,
    Emphasis,
    Strike,
    Code,
    CodeBlock,
    Quote,
    List,
    Item,
    Link(String),
    /// Hidden until the reader reveals it; never inside code or link text.
    Spoiler,
}
#[derive(Clone, Debug, PartialEq)]
pub enum Node {
    Text(String),
    Literal(String),
    Element(Kind, Vec<Node>),
    Break,
}
pub const MAX_DEPTH: usize = 16;
pub const MAX_EVENTS: usize = 16000;
pub fn parse(text: &str) -> Vec<Node> {
    // Enforce the wire limit independently for older/malformed servers and previews.
    let bounded: String = text
        .chars()
        .take(thiscord_shared::chat::MAX_MESSAGE_CHARS)
        .collect();
    let mut stack = vec![(Kind::Plain, Vec::new())];
    let events = Parser::new_ext(&bounded, Options::ENABLE_STRIKETHROUGH).into_offset_iter();
    for (count, (event, range)) in events.enumerate() {
        if count >= MAX_EVENTS {
            return vec![Node::Literal(bounded)];
        }
        match event {
            Event::Start(tag) => {
                if stack.len() >= MAX_DEPTH {
                    return vec![Node::Literal(bounded)];
                }
                let kind = match tag {
                    Tag::Paragraph | Tag::Heading { .. } => Kind::Paragraph,
                    Tag::Strong => Kind::Strong,
                    Tag::Emphasis => Kind::Emphasis,
                    Tag::Strikethrough => Kind::Strike,
                    Tag::CodeBlock(_) => Kind::CodeBlock,
                    Tag::BlockQuote(_) => Kind::Quote,
                    Tag::List(_) => Kind::List,
                    Tag::Item => Kind::Item,
                    Tag::Link { dest_url, .. } => crate::message_links::web_url(&dest_url)
                        .map(|u| Kind::Link(u.into()))
                        .unwrap_or(Kind::Plain),
                    _ => Kind::Plain,
                };
                stack.push((kind, vec![]));
            }
            Event::End(_) => {
                if stack.len() > 1 {
                    let (kind, children) = stack.pop().unwrap();
                    stack
                        .last_mut()
                        .unwrap()
                        .1
                        .push(Node::Element(kind, children));
                }
            }
            // Escaped or entity-decoded text differs from its source; mark it
            // literal so it can never form a spoiler delimiter.
            Event::Text(value) => {
                let nodes = &mut stack.last_mut().unwrap().1;
                let escaped = range.start > 0 && bounded.as_bytes()[range.start - 1] == b'\\';
                if bounded.get(range.clone()) != Some(value.as_ref()) {
                    nodes.push(Node::Literal(value.into_string()));
                } else if escaped {
                    // A backslash escape yields its character as the start of
                    // a text event; only that first character is literal.
                    let split = value.chars().next().map_or(0, char::len_utf8);
                    nodes.push(Node::Literal(value[..split].into()));
                    if split < value.len() {
                        nodes.push(Node::Text(value[split..].into()));
                    }
                } else {
                    nodes.push(Node::Text(value.into_string()));
                }
            }
            Event::Html(value) | Event::InlineHtml(value) => stack
                .last_mut()
                .unwrap()
                .1
                .push(Node::Literal(value.into_string())),
            Event::Code(value) => stack.last_mut().unwrap().1.push(Node::Element(
                Kind::Code,
                vec![Node::Literal(value.into_string())],
            )),
            Event::SoftBreak | Event::HardBreak | Event::Rule => {
                stack.last_mut().unwrap().1.push(Node::Break)
            }
            _ => {}
        }
    }
    spoilers(stack.pop().unwrap().1)
}

fn push_text(nodes: &mut Vec<Node>, text: &str) {
    if let Some(Node::Text(last)) = nodes.last_mut() {
        last.push_str(text);
    } else {
        nodes.push(Node::Text(text.into()));
    }
}

/// Pairs `||` delimiters left to right within each inline sequence. An
/// unmatched final delimiter and empty pairs stay literal text.
fn spoilers(nodes: Vec<Node>) -> Vec<Node> {
    enum Piece {
        Node(Node),
        Delimiter,
    }
    let mut pieces = Vec::new();
    let mut delimiters = 0usize;
    let mut text = String::new();
    let flush = |text: &mut String, pieces: &mut Vec<Piece>, delimiters: &mut usize| {
        let mut rest = text.as_str();
        while let Some(at) = rest.find("||") {
            if at > 0 {
                pieces.push(Piece::Node(Node::Text(rest[..at].into())));
            }
            pieces.push(Piece::Delimiter);
            *delimiters += 1;
            rest = &rest[at + 2..];
        }
        if !rest.is_empty() {
            pieces.push(Piece::Node(Node::Text(rest.into())));
        }
        text.clear();
    };
    for node in nodes {
        match node {
            // Adjacent text events may split a delimiter; merge them first.
            Node::Text(value) => text.push_str(&value),
            Node::Element(kind, children) => {
                flush(&mut text, &mut pieces, &mut delimiters);
                let children = if matches!(kind, Kind::Code | Kind::CodeBlock | Kind::Link(_)) {
                    children
                } else {
                    spoilers(children)
                };
                pieces.push(Piece::Node(Node::Element(kind, children)));
            }
            other => {
                flush(&mut text, &mut pieces, &mut delimiters);
                pieces.push(Piece::Node(other));
            }
        }
    }
    flush(&mut text, &mut pieces, &mut delimiters);
    let mut output = Vec::new();
    let mut open: Option<Vec<Node>> = None;
    let mut remaining = delimiters;
    for piece in pieces {
        match piece {
            Piece::Delimiter => {
                remaining -= 1;
                match open.take() {
                    None if remaining > 0 => open = Some(Vec::new()),
                    None => push_text(&mut output, "||"),
                    Some(inner) if inner.is_empty() => push_text(&mut output, "||||"),
                    Some(inner) => output.push(Node::Element(Kind::Spoiler, inner)),
                }
            }
            Piece::Node(Node::Text(value)) => match &mut open {
                Some(inner) => push_text(inner, &value),
                None => push_text(&mut output, &value),
            },
            Piece::Node(node) => match &mut open {
                Some(inner) => inner.push(node),
                None => output.push(node),
            },
        }
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    fn links(nodes: &[Node]) -> Vec<String> {
        nodes
            .iter()
            .flat_map(|n| match n {
                Node::Element(k, c) => {
                    let mut v = links(c);
                    if let Kind::Link(u) = k {
                        v.push(u.clone());
                    }
                    v
                }
                _ => vec![],
            })
            .collect()
    }
    #[test]
    fn unsafe_destinations_are_never_links() {
        for url in [
            "javascript:alert(1)",
            "data:text/html,x",
            "file:///tmp/a",
            "https://user:pass@example.com",
            "//example.com",
        ] {
            assert!(links(&parse(&format!("[click]({url})"))).is_empty());
        }
        assert_eq!(
            links(&parse("[site](https://example.com/a)")),
            vec!["https://example.com/a"]
        );
    }
    #[test]
    fn raw_html_is_only_literal_and_code_is_preserved() {
        assert!(format!("{:?}", parse("<script>alert(1)</script>")).contains("Literal"));
        let nodes = parse("```rust\nlet x = <tag>;\n```");
        assert!(matches!(&nodes[0], Node::Element(Kind::CodeBlock, _)));
        assert!(matches!(
            &parse("**bold**")[0],
            Node::Element(Kind::Paragraph, _)
        ));
    }
    fn spoiler_texts(nodes: &[Node]) -> Vec<String> {
        fn text(nodes: &[Node]) -> String {
            nodes
                .iter()
                .map(|n| match n {
                    Node::Text(t) | Node::Literal(t) => t.clone(),
                    Node::Element(_, c) => text(c),
                    Node::Break => "
"
                    .into(),
                })
                .collect()
        }
        nodes
            .iter()
            .flat_map(|n| match n {
                Node::Element(Kind::Spoiler, c) => vec![text(c)],
                Node::Element(_, c) => spoiler_texts(c),
                _ => vec![],
            })
            .collect()
    }
    #[test]
    fn spoilers_pair_within_inline_text_and_skip_code_escapes_and_entities() {
        assert_eq!(spoiler_texts(&parse("a ||b|| c")), ["b"]);
        assert_eq!(spoiler_texts(&parse("||one|| and ||two||")), ["one", "two"]);
        assert_eq!(
            spoiler_texts(&parse("||**bold** [x](https://a.b)||")),
            ["bold x"]
        );
        assert_eq!(spoiler_texts(&parse("**||inside||**")), ["inside"]);
        assert_eq!(
            spoiler_texts(&parse(
                "||line
break||"
            )),
            ["line
break"]
        );
        for plain in [
            "||open",
            "||||",
            "|||",
            "`||code||`",
            "```
||block||
```",
            r"\|\|escaped\|\|",
            "&#124;&#124;entity&#124;&#124;",
            "[||link||](https://a.b)",
            "||first paragraph

second||",
        ] {
            assert!(spoiler_texts(&parse(plain)).is_empty(), "{plain}");
        }
        // An odd delimiter count leaves the last one literal.
        let nodes = parse("||a|| b ||");
        assert_eq!(spoiler_texts(&nodes), ["a"]);
        assert!(format!("{nodes:?}").contains("b ||"));
    }
    #[test]
    fn preview_masking_hides_every_rendered_spoiler() {
        for text in [
            "x ||secret|| y",
            "||a|| mid ||b||",
            "**||bold secret||** tail",
            "> ||quoted secret||",
            "- ||listed secret||
- other",
            "||split
secret|| after",
            "start ||a

b|| ||hidden|| end",
        ] {
            let masked = thiscord_shared::chat::mask_spoilers(text);
            for secret in spoiler_texts(&parse(text)) {
                if secret.trim().is_empty() {
                    continue;
                }
                assert!(!masked.contains(secret.trim()), "{text} leaks {secret}");
            }
        }
    }
    #[test]
    fn work_and_nesting_are_bounded() {
        assert_eq!(parse(&"x".repeat(20000)), parse(&"x".repeat(4000)));
        let nested = format!("{}text", "> ".repeat(100));
        assert!(matches!(&parse(&nested)[0], Node::Literal(_)));
    }
}
