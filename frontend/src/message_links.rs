//! Plain-text link detection and the desktop browser command's URL policy.
use linkify::{LinkFinder, LinkKind};

pub struct Span<'a> {
    pub text: &'a str,
    pub href: Option<String>,
}

/// Accept only absolute web URLs, never OS handlers, files or embedded credentials.
pub fn web_url(value: &str) -> Option<url::Url> {
    let (scheme, _) = value.split_once("://")?;
    if !(scheme.eq_ignore_ascii_case("http") || scheme.eq_ignore_ascii_case("https"))
        || value
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '\\')
    {
        return None;
    }
    let url = url::Url::parse(value).ok()?;
    (url.host_str().is_some() && url.username().is_empty() && url.password().is_none())
        .then_some(url)
}

/// Preserve every character; punctuation stays outside anchors. Bare domains use HTTPS.
pub fn spans(text: &str) -> Vec<Span<'_>> {
    let mut finder = LinkFinder::new();
    finder.kinds(&[LinkKind::Url]).url_must_have_scheme(false);
    finder
        .spans(text)
        .map(|span| {
            let text = span.as_str();
            let href = (span.kind() == Some(&LinkKind::Url))
                .then(|| {
                    if text.contains("://") {
                        web_url(text)
                    } else {
                        web_url(&format!("https://{text}"))
                    }
                })
                .flatten()
                .map(String::from);
            Span { text, href }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_preserve_unicode_whitespace_and_surrounding_punctuation() {
        let text = "👋 See (https://example.com/a?q=one&lang=tr#part),\nhttps://en.wikipedia.org/wiki/Rust_(programming_language).";
        let parts = spans(text);
        assert_eq!(parts.iter().map(|part| part.text).collect::<String>(), text);
        let links: Vec<_> = parts
            .iter()
            .filter_map(|part| part.href.as_deref())
            .collect();
        assert_eq!(
            links,
            [
                "https://example.com/a?q=one&lang=tr#part",
                "https://en.wikipedia.org/wiki/Rust_(programming_language)",
            ]
        );
    }

    #[test]
    fn unicode_urls_keep_their_label_and_encode_the_destination() {
        let parts = spans("Read https://bücher.example/über.");
        let link = parts.iter().find(|part| part.href.is_some()).unwrap();
        assert_eq!(link.text, "https://bücher.example/über");
        assert_eq!(
            link.href.as_deref(),
            Some("https://xn--bcher-kva.example/%C3%BCber")
        );
    }

    #[test]
    fn bare_domains_use_https_and_http_is_preserved() {
        let parts = spans(
            "www.google.com google.com/search?q=hello www.example.com thiscord.com.tr http://localhost:3000/test HTTPS://example.org",
        );
        let links: Vec<_> = parts
            .iter()
            .filter_map(|part| part.href.as_deref())
            .collect();
        assert_eq!(
            links,
            [
                "https://www.google.com/",
                "https://google.com/search?q=hello",
                "https://www.example.com/",
                "https://thiscord.com.tr/",
                "http://localhost:3000/test",
                "https://example.org/",
            ]
        );
    }

    #[test]
    fn desktop_rejects_non_web_urls_credentials_and_malformed_input() {
        for value in [
            "javascript:alert(1)",
            "data:text/html,<script>alert(1)</script>",
            "file:///C:/Windows/explorer.exe",
            "ftp://example.com/file",
            "mailto:user@example.com",
            "ms-settings:privacy",
            "//example.com",
            "example.com",
            "https://",
            "https://user:password@example.com",
            "https://example.com\n",
            "https://example.com\\file",
        ] {
            assert!(web_url(value).is_none(), "accepted {value:?}");
        }
        assert!(web_url("https://example.com/a?b=1&c=2#fragment").is_some());
        assert!(web_url("http://[::1]:3000/path").is_some());
    }

    #[test]
    fn markup_and_unsupported_schemes_remain_text() {
        let text = "<script>alert('x')</script> javascript:alert(1) data:text/html,bad file:///tmp/test ftp://example.com/a";
        let parts = spans(text);
        assert_eq!(parts.iter().map(|part| part.text).collect::<String>(), text);
        assert!(parts.iter().all(|part| part.href.is_none()));
        assert!(spans("").iter().all(|part| part.href.is_none()));
    }
}
