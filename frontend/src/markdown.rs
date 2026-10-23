//! Bounded Markdown AST; the UI constructs DOM nodes and never accepts raw HTML.
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
    for (count, event) in Parser::new_ext(&bounded, Options::ENABLE_STRIKETHROUGH).enumerate() {
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
            Event::Text(value) => stack
                .last_mut()
                .unwrap()
                .1
                .push(Node::Text(value.into_string())),
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
    stack.pop().unwrap().1
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
    #[test]
    fn work_and_nesting_are_bounded() {
        assert_eq!(parse(&"x".repeat(20000)), parse(&"x".repeat(4000)));
        let nested = format!("{}text", "> ".repeat(100));
        assert!(matches!(&parse(&nested)[0], Node::Literal(_)));
    }
}
