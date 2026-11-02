//! Invite URL validation shared by the web landing page, web client and native launcher.
use thiscord_shared::account::WEB_APP_PATH;
use url::Url;

/// Fragment prefix the web client reads at `/app/#invite=CODE`. Like
/// `/invite#CODE`, the fragment is never sent to servers or written to logs.
const WEB_FRAGMENT: &str = "invite=";

pub fn valid_code(code: &str) -> bool {
    code.len() == 43
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

fn origin(value: &str) -> Option<Url> {
    let url = Url::parse(value).ok()?;
    (matches!(url.scheme(), "http" | "https")
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        && url.path() == "/")
        .then_some(url)
}

pub fn desktop_link(base: &str, code: &str) -> Option<String> {
    let base = origin(base)?;
    if !valid_code(code) {
        return None;
    }
    let mut link = Url::parse("thiscord://invite").ok()?;
    link.query_pairs_mut()
        .append_pair("origin", &base.origin().ascii_serialization());
    link.set_fragment(Some(code));
    Some(link.into())
}

/// Never let a link select the API backend or supply credentials.
pub fn desktop_code(link: &str, base: &str) -> Option<String> {
    if link.len() > 2048 {
        return None;
    }
    let link = Url::parse(link).ok()?;
    if link.scheme() != "thiscord"
        || link.host_str() != Some("invite")
        // Windows URI handling may canonicalize an empty authority path to `/`.
        // Both identify the invite host; no other paths are accepted.
        || !matches!(link.path(), "" | "/")
        || link.port().is_some()
        || !link.username().is_empty()
        || link.password().is_some()
    {
        return None;
    }
    let pairs: Vec<_> = link.query_pairs().collect();
    if pairs.len() != 1
        || pairs[0].0 != "origin"
        || origin(&pairs[0].1)?.origin() != origin(base)?.origin()
    {
        return None;
    }
    let code = link.fragment()?;
    valid_code(code).then(|| code.to_owned())
}

/// Same-origin path that opens the web client with a pending invitation.
pub fn web_app_link(code: &str) -> Option<String> {
    valid_code(code).then(|| format!("{WEB_APP_PATH}/#{WEB_FRAGMENT}{code}"))
}

/// The pending invitation in a web client `location.hash`, if complete.
pub fn fragment_code(hash: &str) -> Option<String> {
    let code = hash
        .strip_prefix('#')
        .unwrap_or(hash)
        .strip_prefix(WEB_FRAGMENT)?;
    valid_code(code).then(|| code.to_owned())
}

pub fn input_code(input: &str, base: &str) -> Option<String> {
    let input = input.trim();
    if valid_code(input) {
        return Some(input.to_owned());
    }
    if input.starts_with("thiscord:") {
        return desktop_code(input, base);
    }
    let link = Url::parse(input).ok()?;
    if link.origin() != origin(base)?.origin()
        || link.query().is_some()
        || !link.username().is_empty()
        || link.password().is_some()
    {
        return None;
    }
    let fragment = link.fragment()?;
    if link.path() == "/invite" {
        valid_code(fragment).then(|| fragment.to_owned())
    } else if link.path().trim_end_matches('/') == WEB_APP_PATH {
        fragment_code(fragment)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const CODE: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
    #[test]
    fn accepts_windows_canonicalized_desktop_invites() {
        let base = "https://thiscord.com.tr";
        let link = format!("thiscord://invite/?origin=https%3A%2F%2Fthiscord.com.tr#{CODE}");
        assert_eq!(desktop_code(&link, base), Some(CODE.into()));
        assert_eq!(input_code(&link, base), Some(CODE.into()));
        assert!(desktop_code(&link, "https://other.example").is_none());
        for path in ["//", "/other", "/invite"] {
            assert!(desktop_code(&link.replace("/?", &format!("{path}?")), base).is_none());
        }
    }
    #[test]
    fn handoff_round_trip_and_origin_isolation() {
        let link = desktop_link("https://thiscord.com.tr", CODE).unwrap();
        assert_eq!(
            desktop_code(&link, "https://thiscord.com.tr/"),
            Some(CODE.into())
        );
        assert!(desktop_code(&link, "https://other.example").is_none());
        assert!(desktop_code(&link, "http://thiscord.com.tr").is_none());
        assert!(
            desktop_code(
                &link.replace("?origin=", "/other?origin="),
                "https://thiscord.com.tr"
            )
            .is_none()
        );
        assert!(
            desktop_code(
                &link.replace("#", "&origin=https://other.example#"),
                "https://thiscord.com.tr"
            )
            .is_none()
        );
        assert!(
            desktop_code(
                &link.replace("//invite", "//user@invite"),
                "https://thiscord.com.tr"
            )
            .is_none()
        );
        assert!(desktop_link("file:///", CODE).is_none());
        assert!(desktop_link("https://thiscord.com.tr", "bad").is_none());
    }
    #[test]
    fn web_app_fragment_round_trip() {
        let link = web_app_link(CODE).unwrap();
        assert_eq!(link, format!("/app/#invite={CODE}"));
        let hash = link.split_once('#').map(|(_, h)| format!("#{h}")).unwrap();
        assert_eq!(fragment_code(&hash), Some(CODE.into()));
        assert_eq!(fragment_code(&hash[1..]), Some(CODE.into()));
        assert!(web_app_link("bad").is_none());
        for hash in [
            "",
            "#",
            "#invite=",
            "#invite=bad",
            &format!("#{CODE}"),
            &format!("#x{CODE}"),
            &format!("#invite={CODE}&x"),
            "#voice-overlay",
            "#notifications",
        ] {
            assert!(fragment_code(hash).is_none(), "{hash}");
        }
        let base = "https://thiscord.com.tr";
        for path in ["/app/", "/app"] {
            assert_eq!(
                input_code(&format!("{base}{path}#invite={CODE}"), base),
                Some(CODE.into())
            );
        }
        assert!(input_code(&format!("{base}/app/#{CODE}"), base).is_none());
        assert!(input_code(&format!("{base}/app/x#invite={CODE}"), base).is_none());
        assert!(input_code(&format!("https://other.example/app/#invite={CODE}"), base).is_none());
    }
    #[test]
    fn accepts_only_complete_invites_for_this_installation() {
        let base = "http://localhost:3000";
        assert_eq!(
            input_code(&format!("{base}/invite#{CODE}"), base),
            Some(CODE.into())
        );
        assert_eq!(input_code(CODE, base), Some(CODE.into()));
        for link in [
            format!("{base}/invite?code=x#{CODE}"),
            format!("{base}/other#{CODE}"),
            format!("http://localhost:3001/invite#{CODE}"),
            format!("http://user@localhost:3000/invite#{CODE}"),
            format!("{base}/invite#bad"),
        ] {
            assert!(input_code(&link, base).is_none());
        }
    }
}
