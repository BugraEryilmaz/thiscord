//! Bounded, content-free record of local WebView video bridge attempts.
//! Kept in memory for remote support; nothing is written to disk or logged.
use super::preview::Preview;
use crate::video_bridge::{Pair, address_class, pair_facts};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex, Weak},
    time::Instant,
};
use thiscord_shared::video_bridge::*;

/// The desktop's half of one record, written by the peer and its relay.
pub struct Recorder {
    started: Instant,
    section: Mutex<Section>,
}
impl Recorder {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            section: Default::default(),
        })
    }
    pub fn event(&self, text: impl Into<String>) {
        let at = self.started.elapsed().as_millis() as u64;
        if let Ok(mut section) = self.section.lock() {
            section.event(at, text);
        }
    }
    pub fn fact(&self, key: &str, value: impl ToString) {
        if let Ok(mut section) = self.section.lock() {
            section.fact(key, value);
        }
    }
    pub fn replace(&self, prefix: &str, facts: BTreeMap<String, String>) {
        if let Ok(mut section) = self.section.lock() {
            section.replace(prefix, facts);
        }
    }
    pub fn apply_stats(&self, facts: &BTreeMap<String, String>) {
        if let Ok(mut section) = self.section.lock() {
            crate::video_bridge::apply_stats(&mut section, facts);
        }
    }
    pub(crate) fn snapshot(&self) -> Section {
        self.section.lock().map(|s| s.clone()).unwrap_or_default()
    }
}

struct Entry {
    id: u64,
    kind: Kind,
    outcome: Outcome,
    webview: Section,
    desktop: Arc<Recorder>,
    preview: Weak<Preview>,
}
static LOG: Mutex<VecDeque<Entry>> = Mutex::new(VecDeque::new());

fn entry(log: &mut VecDeque<Entry>, id: u64, kind: Kind) -> &mut Entry {
    if let Some(index) = log.iter().position(|e| e.id == id) {
        return &mut log[index];
    }
    if log.len() >= MAX_RECORDS {
        log.pop_front();
    }
    log.push_back(Entry {
        id,
        kind,
        outcome: Outcome::Pending,
        webview: Section::default(),
        desktop: Recorder::new(),
        preview: Weak::new(),
    });
    log.back_mut().expect("just pushed")
}

/// Starts (or rejoins) the desktop section for a WebView-chosen record ID.
/// ID zero comes from WebViews that predate diagnostics and is not retained.
pub fn open(id: u64, kind: Kind) -> Arc<Recorder> {
    if id == 0 {
        return Recorder::new();
    }
    let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    entry(&mut log, id, kind).desktop.clone()
}
pub fn attach(id: u64, preview: &Arc<Preview>) {
    let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(entry) = log.iter_mut().find(|e| e.id == id) {
        entry.preview = Arc::downgrade(preview);
    }
}
pub fn update(update: Update) {
    if update.id == 0 {
        return;
    }
    let mut log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    let entry = entry(&mut log, update.id, update.kind);
    entry.outcome = update.outcome;
    entry.webview = update.webview;
}

/// Samples live peers first, so open attempts include current ICE state.
pub async fn records() -> Vec<Record> {
    let live: Vec<_> = {
        let log = LOG.lock().unwrap_or_else(|e| e.into_inner());
        log.iter()
            .filter_map(|e| Some((e.preview.upgrade()?, e.desktop.clone())))
            .collect()
    };
    for (preview, recorder) in live {
        preview.sample(&recorder).await;
    }
    let log = LOG.lock().unwrap_or_else(|e| e.into_inner());
    log.iter()
        .map(|e| Record {
            id: e.id,
            kind: e.kind,
            age_ms: e.desktop.started.elapsed().as_millis() as u64,
            outcome: e.outcome,
            webview: e.webview.clone(),
            desktop: e.desktop.snapshot(),
        })
        .collect()
}

/// Whitelisted scalars from a native WebRTC report. Candidate addresses are
/// reduced to classes; IDs, credentials and certificates are dropped.
pub fn stats_facts(report: &rtc::statistics::report::RTCStatsReport) -> BTreeMap<String, String> {
    use rtc::statistics::report::RTCStatsReportEntry as Entry;
    let text = |value: &serde_json::Value, key: &str| match value.get(key) {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) => v.to_string(),
        None => String::new(),
    };
    let count =
        |value: &serde_json::Value, key: &str| value.get(key).and_then(|v| v.as_u64()).unwrap_or(0);
    let mut candidates = BTreeMap::new();
    let mut pairs = Vec::new();
    let mut facts = BTreeMap::new();
    let mut summary = [BTreeMap::<String, usize>::new(), BTreeMap::new()];
    for entry in report.iter() {
        let (remote, value) = match entry {
            Entry::LocalCandidate(s) => (false, serde_json::to_value(s)),
            Entry::RemoteCandidate(s) => (true, serde_json::to_value(s)),
            _ => continue,
        };
        let Ok(value) = value else { continue };
        let address = value.get("address").and_then(|v| v.as_str()).unwrap_or("");
        let description = format!(
            "{} {} {}",
            text(&value, "protocol"),
            text(&value, "candidateType"),
            address_class(address)
        );
        *summary[usize::from(remote)]
            .entry(description.clone())
            .or_default() += 1;
        // Candidate stats are named `RTCLocalIceCandidate_<id>`; pairs use `<id>`.
        let id = text(&value, "id");
        if let Some((_, short)) = id.split_once('_') {
            candidates.insert(short.to_owned(), description.clone());
        }
        candidates.insert(id, description);
    }
    for entry in report.iter() {
        match entry {
            Entry::IceCandidatePair(s) => {
                let Ok(value) = serde_json::to_value(s) else {
                    continue;
                };
                let side = |key| {
                    candidates
                        .get(&text(&value, key))
                        .cloned()
                        .unwrap_or_else(|| "unknown".into())
                };
                pairs.push(Pair {
                    state: text(&value, "state"),
                    nominated: value
                        .get("nominated")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    local: side("localCandidateId"),
                    remote: side("remoteCandidateId"),
                    requests_sent: count(&value, "requestsSent"),
                    requests_received: count(&value, "requestsReceived"),
                    responses_sent: count(&value, "responsesSent"),
                    responses_received: count(&value, "responsesReceived"),
                    bytes_sent: count(&value, "bytesSent"),
                    bytes_received: count(&value, "bytesReceived"),
                });
            }
            Entry::Transport(s) => {
                let Ok(value) = serde_json::to_value(s) else {
                    continue;
                };
                for (key, field) in [
                    ("transport.ice_state", "iceState"),
                    ("transport.dtls_state", "dtlsState"),
                    ("transport.dtls_role", "dtlsRole"),
                    ("transport.ice_role", "iceRole"),
                    (
                        "transport.selected_pair_changes",
                        "selectedCandidatePairChanges",
                    ),
                    ("transport.packets_sent", "packetsSent"),
                    ("transport.packets_received", "packetsReceived"),
                ] {
                    facts.insert(key.into(), text(&value, field));
                }
            }
            Entry::OutboundRtp(s) => {
                let Ok(value) = serde_json::to_value(s) else {
                    continue;
                };
                facts.insert("rtp.packets_sent".into(), text(&value, "packetsSent"));
                facts.insert("rtp.bytes_sent".into(), text(&value, "bytesSent"));
            }
            _ => {}
        }
    }
    for (key, counts) in ["candidates.local", "candidates.remote"]
        .into_iter()
        .zip(summary)
    {
        let value = counts
            .into_iter()
            .map(|(k, n)| format!("{k}×{n}"))
            .collect::<Vec<_>>()
            .join(", ");
        facts.insert(
            key.into(),
            if value.is_empty() {
                "none".into()
            } else {
                value
            },
        );
    }
    facts.extend(pair_facts(&pairs));
    facts
}
