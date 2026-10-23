//! Invite URL validation shared by the web landing page and native launcher.
use url::Url;

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
        || !link.path().is_empty()
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
        || link.path() != "/invite"
        || link.query().is_some()
        || !link.username().is_empty()
        || link.password().is_some()
    {
        return None;
    }
    let code = link.fragment()?;
    valid_code(code).then(|| code.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    const CODE: &str = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQ";
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
