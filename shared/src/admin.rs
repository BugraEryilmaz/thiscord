//! Read-only, instance-wide diagnostics. Never contains credentials or message content.
use crate::{ChannelId, GuildId, permissions::InstanceRole, voice::Participant};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const DIAGNOSTICS_PATH: &str = "/api/v1/admin/diagnostics";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostics {
    pub sampled_at: DateTime<Utc>,
    pub role: InstanceRole,
    pub version: String,
    pub uptime_seconds: u64,
    pub host: HostMetrics,
    pub database_connections: u32,
    pub database_idle_connections: u32,
    pub rooms: Vec<RoomDiagnostics>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostMetrics {
    pub sampled_at: Option<DateTime<Utc>>,
    /// Host-wide Linux CPU use since the preceding sample, normalized to 0–100.
    pub cpu_percent: Option<f64>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    pub process_memory_bytes: Option<u64>,
    pub load_average: Option<[f64; 3]>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RoomDiagnostics {
    pub guild_id: GuildId,
    pub guild_name: String,
    pub channel_id: ChannelId,
    pub channel_name: String,
    pub participants: Vec<ParticipantDiagnostics>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ParticipantDiagnostics {
    pub participant: Participant,
    pub network: NetworkMetrics,
    /// Video ingress, ingress drops, egress drops, sent packets, send timeouts.
    pub video_counters: [u64; 5],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct NetworkMetrics {
    pub sampled_at: Option<DateTime<Utc>>,
    /// ICE round-trip time between this participant and the SFU, not mouth-to-ear delay.
    pub round_trip_ms: Option<f64>,
    /// Incoming microphone RTP inter-arrival jitter at the SFU.
    pub microphone_jitter_ms: Option<f64>,
    pub microphone_packets_received: Option<u64>,
    pub microphone_packets_lost: Option<i64>,
}
