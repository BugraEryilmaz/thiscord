//! Shared OS credential namespace for desktop provisioning and the native SDK.
pub fn entry(server: &str, name: &str) -> Result<keyring::Entry, &'static str> {
    if !(3..=32).contains(&name.len())
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err("Invalid bot credential name");
    }
    let url = url::Url::parse(server).map_err(|_| "Invalid server URL")?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    if !(url.scheme() == "https" || url.scheme() == "http" && local)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err("Use an HTTPS server origin, or HTTP on loopback");
    }
    keyring::Entry::new(
        "thiscord-scripting-bots",
        &format!("{}|{name}", url.origin().ascii_serialization()),
    )
    .map_err(|_| "OS credential store unavailable")
}
