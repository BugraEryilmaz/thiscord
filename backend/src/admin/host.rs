use std::{
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};
use thiscord_shared::admin::HostMetrics;

#[derive(Default)]
struct Sampler {
    at: Option<Instant>,
    cpu: Option<(u64, u64)>,
    value: HostMetrics,
}

// Called only on bounded blocking workers. Cache across dashboard viewers.
pub(super) fn sample() -> HostMetrics {
    static STATE: OnceLock<Mutex<Sampler>> = OnceLock::new();
    let mut state = STATE
        .get_or_init(Default::default)
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    if state
        .at
        .is_some_and(|at| at.elapsed() < Duration::from_secs(4))
    {
        return state.value.clone();
    }
    let cpu = std::fs::read_to_string("/proc/stat")
        .ok()
        .and_then(|s| cpu_ticks(&s));
    let cpu_percent = state
        .cpu
        .zip(cpu)
        .and_then(|(old, new)| cpu_usage(old, new));
    let mem = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let total = kib(&mem, "MemTotal:");
    let used = total
        .zip(kib(&mem, "MemAvailable:"))
        .and_then(|(t, a)| t.checked_sub(a));
    let process = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| kib(&s, "VmRSS:"));
    let load = std::fs::read_to_string("/proc/loadavg").ok().and_then(|s| {
        let values: Vec<f64> = s
            .split_whitespace()
            .take(3)
            .map(str::parse)
            .collect::<Result<_, _>>()
            .ok()?;
        values.try_into().ok()
    });
    state.value = HostMetrics {
        sampled_at: cpu.map(|_| chrono::Utc::now()),
        cpu_percent,
        memory_used_bytes: used,
        memory_total_bytes: total,
        process_memory_bytes: process,
        load_average: load,
    };
    state.cpu = cpu;
    state.at = Some(Instant::now());
    state.value.clone()
}
fn kib(text: &str, key: &str) -> Option<u64> {
    text.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        (parts.next()? == key)
            .then(|| parts.next()?.parse::<u64>().ok()?.checked_mul(1024))
            .flatten()
    })
}
fn cpu_ticks(text: &str) -> Option<(u64, u64)> {
    let mut fields = text.lines().next()?.split_whitespace();
    if fields.next()? != "cpu" {
        return None;
    }
    // guest/guest_nice are already included in user/nice; do not double count.
    let values: Vec<u64> = fields
        .take(8)
        .map(str::parse)
        .collect::<Result<_, _>>()
        .ok()?;
    if values.len() < 5 {
        return None;
    }
    Some((values.iter().sum(), values[3] + values[4]))
}
fn cpu_usage(old: (u64, u64), new: (u64, u64)) -> Option<f64> {
    let total = new.0.checked_sub(old.0)?;
    let idle = new.1.checked_sub(old.1)?;
    (total > 0 && idle <= total).then(|| 100.0 * (total - idle) as f64 / total as f64)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn linux_metrics_have_correct_units_and_reset_behavior() {
        assert_eq!(
            cpu_ticks("cpu  10 20 30 40 5 6 7 8 100 200\ncpu0 1"),
            Some((126, 45))
        );
        assert_eq!(cpu_usage((100, 40), (200, 65)), Some(75.0));
        assert_eq!(cpu_usage((200, 65), (100, 40)), None);
        assert_eq!(cpu_usage((100, 40), (100, 40)), None);
        assert_eq!(
            kib("MemTotal:  2048 kB\nMemAvailable: 512 kB", "MemTotal:"),
            Some(2097152)
        );
        assert_eq!(kib("", "MemTotal:"), None);
    }
}
