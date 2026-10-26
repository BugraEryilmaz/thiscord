//! Content-free diagnostics for the desktop's local WebView video bridge.
//! Facts are short scalar strings. Producers classify addresses instead of
//! recording them and never include SDP, ICE credentials, certificates or media.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_RECORDS: usize = 12;
pub const MAX_EVENTS: usize = 64;
pub const MAX_FACTS: usize = 192;
pub const MAX_KEY: usize = 64;
pub const MAX_VALUE: usize = 240;

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Section {
    pub facts: BTreeMap<String, String>,
    /// Milliseconds since the record started, with a short description.
    pub events: Vec<(u64, String)>,
}
impl Section {
    pub fn fact(&mut self, key: impl Into<String>, value: impl ToString) {
        let key = truncate(key.into(), MAX_KEY);
        if self.facts.len() < MAX_FACTS || self.facts.contains_key(&key) {
            self.facts
                .insert(key, truncate(value.to_string(), MAX_VALUE));
        }
    }
    /// Replace every fact under `prefix`, so a fresh stats sample cannot
    /// leave candidate pairs from an older sample behind.
    pub fn replace(&mut self, prefix: &str, facts: BTreeMap<String, String>) {
        self.facts.retain(|key, _| !key.starts_with(prefix));
        for (key, value) in facts {
            self.fact(key, value);
        }
    }
    /// Keeps the start of the timeline and the most recent events.
    pub fn event(&mut self, at_ms: u64, text: impl Into<String>) {
        if self.events.len() >= MAX_EVENTS {
            self.events.remove(MAX_EVENTS / 2);
        }
        self.events.push((at_ms, truncate(text.into(), MAX_VALUE)));
    }
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.facts.len() > MAX_FACTS
            || self.events.len() > MAX_EVENTS
            || self
                .facts
                .iter()
                .any(|(k, v)| k.len() > MAX_KEY || v.len() > MAX_VALUE)
            || self.events.iter().any(|(_, e)| e.len() > MAX_VALUE)
        {
            return Err("Invalid video diagnostics");
        }
        Ok(())
    }
}
fn truncate(mut text: String, limit: usize) -> String {
    if text.len() > limit {
        let mut end = limit;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Screen,
    Camera,
    /// Two WebView peers connected to each other, without the desktop.
    WebViewSelfTest,
    /// A video-free repeat of the production bridge, bound to 127.0.0.1.
    LoopbackSelfTest,
    /// The same desktop peer bound to the computer's network interfaces.
    InterfacesSelfTest,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    #[default]
    Pending,
    Connected,
    Playing,
    TimedOut,
    Failed,
    Closed,
    Error,
}

/// WebView-to-desktop IPC. The desktop owns the record and its own section.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Update {
    pub id: u64,
    pub kind: Kind,
    pub outcome: Outcome,
    pub webview: Section,
}
impl Update {
    pub fn validate(&self) -> Result<(), &'static str> {
        self.webview.validate()
    }
}

/// Opens an isolated self-test peer. No viewer lease, SFU subscription or video.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProbeOffer {
    pub id: u64,
    pub interfaces: bool,
    pub sdp: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub id: u64,
    pub kind: Kind,
    /// Time since the attempt started, when this report was produced.
    pub age_ms: u64,
    pub outcome: Outcome,
    pub webview: Section,
    pub desktop: Section,
}

/// Admin `video_diagnostics` reply. Records are oldest first.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub app_version: String,
    pub system: Section,
    pub adapters: Vec<Section>,
    pub firewall: Section,
    pub loopback: Section,
    pub streams: Vec<Section>,
    pub records: Vec<Record>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sections_stay_bounded_and_keep_both_timeline_ends() {
        let mut section = Section::default();
        for i in 0..MAX_FACTS + 10 {
            section.fact(format!("k{i}"), "é".repeat(MAX_VALUE));
        }
        for i in 0..MAX_EVENTS as u64 + 10 {
            section.event(i, format!("event {i}"));
        }
        section.validate().unwrap();
        assert_eq!(section.facts.len(), MAX_FACTS);
        assert_eq!(section.events.first().unwrap().0, 0);
        assert_eq!(section.events.last().unwrap().0, MAX_EVENTS as u64 + 9);
        section.replace("k1", BTreeMap::from([("k1.new".into(), "1".into())]));
        assert!(!section.facts.contains_key("k10"));
        assert_eq!(section.facts["k1.new"], "1");
    }

    #[test]
    fn updates_reject_oversized_or_unknown_input() {
        let wire = serde_json::json!({"id":1,"kind":"screen","outcome":"timed_out",
            "webview":{"facts":{"a":"b"},"events":[[5,"ice=checking"]]}});
        let update: Update = serde_json::from_value(wire.clone()).unwrap();
        update.validate().unwrap();
        let mut extra = wire;
        extra["sdp"] = "v=0".into();
        assert!(serde_json::from_value::<Update>(extra).is_err());
        let mut long = update;
        long.webview.events.push((1, "x".repeat(MAX_VALUE + 1)));
        assert!(long.validate().is_err());
    }
}
