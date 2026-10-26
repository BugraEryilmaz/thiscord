//! Shared interpretation of local video bridge diagnostics. The WebView, the
//! desktop and the admin dashboard use the same address classes and fact keys.
use std::{collections::BTreeMap, net::IpAddr};
use thiscord_shared::video_bridge::*;

/// Sum of ICE connectivity checks across all candidate pairs of one peer.
pub const REQUESTS_SENT: &str = "checks.requests_sent";
pub const REQUESTS_RECEIVED: &str = "checks.requests_received";
pub const RESPONSES_SENT: &str = "checks.responses_sent";
pub const RESPONSES_RECEIVED: &str = "checks.responses_received";
pub const PAIRS: &str = "checks.pairs";
pub const DTLS_STATE: &str = "transport.dtls_state";
pub const ICE_STATE: &str = "transport.ice_state";
pub const OFFER_UDP: &str = "offer.udp_candidates";
pub const OFFER_CANDIDATES: &str = "offer.candidates";
pub const ERROR: &str = "error";

/// Describes an address without revealing it. Loopback is reported as such.
pub fn address_class(address: &str) -> &'static str {
    let address = address.trim().trim_start_matches('[').trim_end_matches(']');
    if address.is_empty() {
        return "hidden";
    }
    if address.ends_with(".local") {
        return "mdns";
    }
    match address.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => {
            let [a, b, ..] = v4.octets();
            if v4.is_loopback() {
                "loopback-v4"
            } else if v4.is_unspecified() {
                "unspecified-v4"
            } else if v4.is_private() {
                "private-v4"
            } else if v4.is_link_local() {
                "link-local-v4"
            } else if a == 100 && (64..128).contains(&b) {
                "cgnat-v4"
            } else {
                "public-v4"
            }
        }
        Ok(IpAddr::V6(v6)) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return address_class(&v4.to_string());
            }
            let first = v6.segments()[0];
            if v6.is_loopback() {
                "loopback-v6"
            } else if v6.is_unspecified() {
                "unspecified-v6"
            } else if first & 0xffc0 == 0xfe80 {
                "link-local-v6"
            } else if first & 0xfe00 == 0xfc00 {
                "ula-v6"
            } else {
                "global-v6"
            }
        }
        Err(_) => "hostname",
    }
}

/// Counts RTP component candidates as `protocol/type/address-class×N`.
pub fn candidate_summary(sdp: &str) -> (String, usize) {
    let mut counts = BTreeMap::<String, usize>::new();
    let mut udp = 0;
    for line in sdp.lines() {
        let Some(rest) = line.trim().strip_prefix("a=candidate:") else {
            continue;
        };
        let fields: Vec<_> = rest.split_whitespace().collect();
        if fields.len() < 8 || fields[1] != "1" {
            continue;
        }
        let protocol = fields[2].to_ascii_lowercase();
        let kind = fields
            .iter()
            .position(|f| *f == "typ")
            .and_then(|i| fields.get(i + 1))
            .copied()
            .unwrap_or("unknown");
        udp += usize::from(protocol == "udp");
        *counts
            .entry(format!("{protocol}/{kind}/{}", address_class(fields[4])))
            .or_default() += 1;
    }
    let text = if counts.is_empty() {
        "none".into()
    } else {
        counts
            .into_iter()
            .map(|(k, n)| format!("{k}×{n}"))
            .collect::<Vec<_>>()
            .join(", ")
    };
    (text, udp)
}

/// Video codec names offered in an SDP, e.g. `H264×6, VP8, VP9`.
pub fn codec_summary(sdp: &str) -> String {
    let mut counts = BTreeMap::<String, usize>::new();
    for line in sdp.lines() {
        if let Some((_, codec)) = line
            .trim()
            .strip_prefix("a=rtpmap:")
            .and_then(|r| r.split_once(' '))
        {
            let name = codec.split('/').next().unwrap_or_default().to_owned();
            if !matches!(name.as_str(), "rtx" | "red" | "ulpfec" | "flexfec-03") {
                *counts.entry(name).or_default() += 1;
            }
        }
    }
    counts
        .into_iter()
        .map(|(k, n)| if n > 1 { format!("{k}×{n}") } else { k })
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fact groups owned by stats samples. Each sample replaces the whole group.
pub const STAT_PREFIXES: [&str; 5] = ["pair.", "checks.", "transport.", "candidates.", "rtp."];
pub fn apply_stats(section: &mut Section, facts: &BTreeMap<String, String>) {
    for prefix in STAT_PREFIXES {
        section.replace(
            prefix,
            facts
                .iter()
                .filter(|(k, _)| k.starts_with(prefix))
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect(),
        );
    }
}

/// Normalized candidate-pair fields shared by browser and native stats.
#[derive(Default)]
pub struct Pair {
    pub state: String,
    pub nominated: bool,
    pub local: String,
    pub remote: String,
    pub requests_sent: u64,
    pub requests_received: u64,
    pub responses_sent: u64,
    pub responses_received: u64,
    pub bytes_sent: u64,
    pub bytes_received: u64,
}
/// Writes at most eight pairs plus totals under `pair.` and `checks.`.
pub fn pair_facts(pairs: &[Pair]) -> BTreeMap<String, String> {
    let mut facts = BTreeMap::new();
    let mut totals = [0u64; 4];
    for pair in pairs {
        totals[0] += pair.requests_sent;
        totals[1] += pair.requests_received;
        totals[2] += pair.responses_sent;
        totals[3] += pair.responses_received;
    }
    for (i, pair) in pairs.iter().take(8).enumerate() {
        facts.insert(
            format!("pair.{i}"),
            format!(
                "{} {}→{} nominated={} req {}/{} resp {}/{} bytes {}/{}",
                pair.state,
                pair.local,
                pair.remote,
                pair.nominated,
                pair.requests_sent,
                pair.requests_received,
                pair.responses_sent,
                pair.responses_received,
                pair.bytes_sent,
                pair.bytes_received
            ),
        );
    }
    for (key, value) in [
        (REQUESTS_SENT, totals[0]),
        (REQUESTS_RECEIVED, totals[1]),
        (RESPONSES_SENT, totals[2]),
        (RESPONSES_RECEIVED, totals[3]),
        (PAIRS, pairs.len() as u64),
    ] {
        facts.insert(key.into(), value.to_string());
    }
    facts
}

pub fn kind_label(kind: Kind) -> &'static str {
    match kind {
        Kind::Screen => "Screen viewer",
        Kind::Camera => "Camera viewer",
        Kind::WebViewSelfTest => "Self-test: WebView to WebView",
        Kind::LoopbackSelfTest => "Self-test: WebView to desktop (127.0.0.1)",
        Kind::InterfacesSelfTest => "Self-test: WebView to desktop (network interfaces)",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Good,
    Note,
    Problem,
}

fn number(section: &Section, key: &str) -> Option<u64> {
    section.facts.get(key)?.parse().ok()
}

/// Ordered findings for the admin dashboard, most specific first.
pub fn verdict(report: &Report) -> Vec<(Severity, String)> {
    let mut findings = Vec::new();
    let viewers: Vec<_> = report
        .records
        .iter()
        .filter(|r| matches!(r.kind, Kind::Screen | Kind::Camera))
        .collect();
    let finished = |r: &&&Record| r.outcome != Outcome::Pending || r.age_ms > 12_000;
    match viewers.iter().rev().find(finished) {
        None => findings.push((
            Severity::Note,
            "No video viewer attempts recorded since the desktop app started. Ask the user to open a stream for 15 seconds, then refresh.".into(),
        )),
        Some(last) => findings.extend(attempt_findings(last)),
    }
    let playing = viewers
        .iter()
        .filter(|r| r.outcome == Outcome::Playing)
        .count();
    let stuck = viewers
        .iter()
        .filter(|r| {
            matches!(
                r.outcome,
                Outcome::TimedOut | Outcome::Failed | Outcome::Pending
            )
        })
        .count();
    if !viewers.is_empty() {
        findings.push((
            Severity::Note,
            format!(
                "{} recorded viewer attempts: {playing} played, {stuck} never connected or still pending.",
                viewers.len()
            ),
        ));
    }
    let latest = |kind| report.records.iter().rev().find(|r| r.kind == kind);
    let connected = |r: &Record| matches!(r.outcome, Outcome::Connected | Outcome::Playing);
    let webview = latest(Kind::WebViewSelfTest);
    let loopback = latest(Kind::LoopbackSelfTest);
    let interfaces = latest(Kind::InterfacesSelfTest);
    if let Some(test) = webview {
        findings.push(if connected(test) {
            (Severity::Good, "WebView WebRTC can connect two of its own peers.".into())
        } else {
            (
                Severity::Problem,
                "The WebView could not connect two of its own WebRTC peers: its WebRTC networking is broken or restricted on this computer, independent of the desktop.".into(),
            )
        });
    }
    match (loopback, interfaces) {
        (Some(l), Some(i)) if !connected(l) && connected(i) => findings.push((
            Severity::Problem,
            "Self-test: the WebView cannot reach the desktop on 127.0.0.1, but can through the computer's network interfaces. Binding the bridge to interface addresses should fix playback here.".into(),
        )),
        (Some(l), Some(i)) if !connected(l) && !connected(i) => findings.push((
            Severity::Problem,
            "Self-test: the WebView reached the desktop through neither 127.0.0.1 nor the network interfaces.".into(),
        )),
        (Some(l), _) if connected(l) => findings.push((
            Severity::Note,
            "Self-test: an isolated 127.0.0.1 bridge connected, so failures are intermittent or specific to live viewers.".into(),
        )),
        _ => {}
    }
    for (key, value) in &report.loopback.facts {
        if !value.starts_with("ok") {
            findings.push((
                Severity::Problem,
                format!("Desktop UDP loopback test {key}: {value}"),
            ));
        }
    }
    if let Some(policies) = report.system.facts.get("webview2.policies") {
        findings.push((
            Severity::Problem,
            format!("WebView2/Edge policies can change WebRTC networking: {policies}"),
        ));
    }
    if let Some(args) = report.system.facts.get("webview2.env_arguments") {
        findings.push((
            Severity::Problem,
            format!("WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS is set: {args}"),
        ));
    }
    let blocks: Vec<_> = report
        .firewall
        .facts
        .iter()
        .filter(|(k, _)| k.starts_with("rules.block."))
        .map(|(_, v)| v.as_str())
        .collect();
    if !blocks.is_empty() {
        findings.push((
            Severity::Problem,
            format!(
                "Windows Firewall blocks Thiscord or WebView2 (often from dismissing the first-run network prompt): {}",
                blocks.join("; ")
            ),
        ));
    }
    if let Some(products) = report.firewall.facts.get("security_products") {
        findings.push((
            Severity::Note,
            format!("Registered security products: {products}"),
        ));
    }
    let tunnels: Vec<_> = report
        .adapters
        .iter()
        .filter(|a| a.facts.get("status").is_some_and(|s| s == "up"))
        .filter(|a| a.facts.get("vpn_like").is_some_and(|v| v == "true"))
        .filter_map(|a| a.facts.get("description").cloned())
        .collect();
    if !tunnels.is_empty() {
        findings.push((
            Severity::Note,
            format!("Active VPN/tunnel-like adapters: {}", tunnels.join("; ")),
        ));
    }
    findings
}

fn attempt_findings(record: &Record) -> Vec<(Severity, String)> {
    let w = &record.webview;
    let d = &record.desktop;
    let label = kind_label(record.kind);
    if record.outcome == Outcome::Playing {
        return vec![(
            Severity::Good,
            format!("Latest {label} attempt played video."),
        )];
    }
    if let Some(error) = w.facts.get(ERROR).or_else(|| d.facts.get(ERROR)) {
        return vec![(
            Severity::Problem,
            format!("Latest {label} attempt failed during setup: {error}"),
        )];
    }
    if record.outcome == Outcome::Connected {
        let frames = d
            .facts
            .get("stream.preview_frames")
            .map_or("unknown", String::as_str);
        return vec![(
            Severity::Problem,
            format!(
                "Latest {label} bridge connected but no video was shown. Frames forwarded to the WebView: {frames}. Check the stream rows for SFU packets and the WebView decoder fields."
            ),
        )];
    }
    let mut findings = Vec::new();
    if number(w, OFFER_UDP) == Some(0) {
        findings.push((
            Severity::Problem,
            format!(
                "The WebView offered no UDP candidates ({}). WebRTC UDP appears disabled or restricted by policy/proxy settings.",
                w.facts.get(OFFER_CANDIDATES).map_or("none", String::as_str)
            ),
        ));
    }
    let sent = number(w, REQUESTS_SENT);
    let desktop_received = number(d, REQUESTS_RECEIVED);
    let desktop_answered = number(d, RESPONSES_SENT);
    let webview_answered = number(w, RESPONSES_RECEIVED);
    let pairs = number(w, PAIRS);
    let dtls = w.facts.get(DTLS_STATE).or_else(|| d.facts.get(DTLS_STATE));
    if pairs == Some(0) {
        findings.push((
            Severity::Problem,
            "The WebView formed no candidate pair with the desktop's 127.0.0.1 address (no compatible local socket, e.g. only IPv6 or TCP).".into(),
        ));
    } else if sent.unwrap_or(0) > 0 && desktop_received == Some(0) {
        findings.push((
            Severity::Problem,
            format!("The WebView sent {} connectivity checks to 127.0.0.1 but the desktop received none. Loopback UDP from the WebView process is being dropped.", sent.unwrap_or(0)),
        ));
    } else if desktop_answered.unwrap_or(0) > 0 && webview_answered == Some(0) {
        findings.push((
            Severity::Problem,
            "The desktop answered the WebView's checks, but no answer reached the WebView. The return path is being dropped.".into(),
        ));
    } else if webview_answered.unwrap_or(0) > 0
        && dtls.is_some_and(|s| !s.eq_ignore_ascii_case("connected"))
    {
        findings.push((
            Severity::Problem,
            format!(
                "ICE connectivity worked but the encryption handshake did not finish (DTLS {}).",
                dtls.map_or("unknown", String::as_str)
            ),
        ));
    } else if sent == Some(0) {
        findings.push((
            Severity::Problem,
            "The WebView never sent connectivity checks to the desktop.".into(),
        ));
    }
    if findings.is_empty() {
        findings.push((
            Severity::Problem,
            format!(
                "Latest {label} attempt ended as {:?} without a specific cause; see the timelines below.",
                record.outcome
            ),
        ));
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_are_classified_not_exposed() {
        for (address, class) in [
            ("127.0.0.1", "loopback-v4"),
            ("192.168.1.4", "private-v4"),
            ("100.72.0.1", "cgnat-v4"),
            ("169.254.3.3", "link-local-v4"),
            ("8.8.8.8", "public-v4"),
            ("::1", "loopback-v6"),
            ("[fe80::1]", "link-local-v6"),
            ("fd00::2", "ula-v6"),
            ("2001:db8::1", "global-v6"),
            ("::ffff:10.0.0.1", "private-v4"),
            ("49148cf1-834c-4608-b7a4-fe2912e5ce86.local", "mdns"),
            ("", "hidden"),
        ] {
            assert_eq!(address_class(address), class, "{address}");
        }
    }

    #[test]
    fn sdp_summaries_count_rtp_candidates_and_codecs() {
        let sdp = "a=rtpmap:96 VP8/90000\r\na=rtpmap:97 rtx/90000\r\na=rtpmap:102 H264/90000\r\na=rtpmap:104 H264/90000\r\n\
            a=candidate:1 1 udp 2113937151 abc.local 63059 typ host generation 0\r\n\
            a=candidate:1 2 udp 2113937151 abc.local 63060 typ host generation 0\r\n\
            a=candidate:2 1 tcp 1509959935 abc.local 9 typ host tcptype active\r\n\
            a=candidate:3 1 udp 2130706431 127.0.0.1 51253 typ host\r\n";
        let (text, udp) = candidate_summary(sdp);
        assert_eq!(
            text,
            "tcp/host/mdns×1, udp/host/loopback-v4×1, udp/host/mdns×1"
        );
        assert_eq!(udp, 2);
        assert!(!text.contains("127.0.0.1") && !text.contains("abc"));
        assert_eq!(codec_summary(sdp), "H264×2, VP8");
    }

    fn record(outcome: Outcome, webview: &[(&str, &str)], desktop: &[(&str, &str)]) -> Record {
        let section = |facts: &[(&str, &str)]| Section {
            facts: facts
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
            events: vec![],
        };
        Record {
            id: 1,
            kind: Kind::Screen,
            age_ms: 20_000,
            outcome,
            webview: section(webview),
            desktop: section(desktop),
        }
    }

    #[test]
    fn verdict_names_the_failing_hop() {
        let report = |r| Report {
            records: vec![r],
            ..Default::default()
        };
        let first = |r| verdict(&report(r))[0].1.clone();
        assert!(
            first(record(
                Outcome::TimedOut,
                &[
                    (PAIRS, "2"),
                    (REQUESTS_SENT, "12"),
                    (RESPONSES_RECEIVED, "0")
                ],
                &[(REQUESTS_RECEIVED, "0"), (RESPONSES_SENT, "0")],
            ))
            .contains("desktop received none")
        );
        assert!(
            first(record(
                Outcome::TimedOut,
                &[
                    (PAIRS, "1"),
                    (REQUESTS_SENT, "12"),
                    (RESPONSES_RECEIVED, "0")
                ],
                &[(REQUESTS_RECEIVED, "12"), (RESPONSES_SENT, "12")],
            ))
            .contains("return path")
        );
        assert!(
            first(record(
                Outcome::Failed,
                &[
                    (PAIRS, "1"),
                    (REQUESTS_SENT, "3"),
                    (RESPONSES_RECEIVED, "3"),
                    (DTLS_STATE, "failed")
                ],
                &[(REQUESTS_RECEIVED, "3"), (RESPONSES_SENT, "3")],
            ))
            .contains("DTLS failed")
        );
        assert!(
            first(record(
                Outcome::TimedOut,
                &[(OFFER_UDP, "0"), (PAIRS, "0")],
                &[]
            ))
            .contains("no UDP candidates")
        );
        assert_eq!(
            verdict(&report(record(Outcome::Playing, &[], &[])))[0].0,
            Severity::Good
        );
        let mut fix = Report::default();
        for (kind, outcome) in [
            (Kind::LoopbackSelfTest, Outcome::TimedOut),
            (Kind::InterfacesSelfTest, Outcome::Connected),
        ] {
            let mut r = record(outcome, &[], &[]);
            r.kind = kind;
            fix.records.push(r);
        }
        assert!(
            verdict(&fix)
                .iter()
                .any(|(_, f)| f.contains("should fix playback"))
        );
    }
}
