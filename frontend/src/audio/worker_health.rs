use super::*;
use std::sync::atomic::AtomicU64;

pub(super) const IDLE: u8 = 0;
pub(super) const START: u8 = 1;
pub(super) const SETTINGS: u8 = 2;
pub(super) const STOP: u8 = 3;
pub(super) const MEDIA: u8 = 4;
pub(super) const STATUS: u8 = 5;
pub(super) const PROCESS: u8 = 6;
pub(super) const RECOVERY: u8 = 7;
pub(super) const OTHER: u8 = 8;
fn name(code: u64) -> &'static str {
    [
        "idle",
        "start",
        "settings",
        "stop",
        "media",
        "status",
        "process_audio",
        "recover_devices",
        "control",
    ]
    .get(code as usize)
    .copied()
    .unwrap_or("unknown")
}
pub(super) struct Probe {
    start: Instant,
    heartbeat: AtomicU64,
    operation: AtomicU64,
    alive: AtomicBool,
    pub active: AtomicBool,
    pub packet_drops: AtomicU64,
    pub restarts: AtomicU64,
    max_processing_us: AtomicU64,
}
impl Probe {
    pub fn new() -> Arc<Self> {
        let probe = Arc::new(Self {
            start: Instant::now(),
            heartbeat: AtomicU64::new(0),
            operation: AtomicU64::new(0),
            alive: AtomicBool::new(true),
            active: AtomicBool::new(false),
            packet_drops: AtomicU64::new(0),
            restarts: AtomicU64::new(0),
            max_processing_us: AtomicU64::new(0),
        });
        let weak = Arc::downgrade(&probe);
        let _ = thread::Builder::new()
            .name("thiscord-audio-watchdog".into())
            .spawn(move || {
                let mut ticks = 0u64;
                let mut stalled = false;
                loop {
                    thread::sleep(Duration::from_secs(1));
                    let Some(probe) = weak.upgrade() else {
                        break;
                    };
                    if !probe.alive.load(Ordering::Acquire) {
                        break;
                    }
                    ticks += 1;
                    let now_stalled = probe.age() >= 3000;
                    if now_stalled != stalled || ticks.is_multiple_of(10) {
                        diagnostics::event(
                            if now_stalled {
                                "worker_stalled"
                            } else {
                                "worker_heartbeat"
                            },
                            probe.snapshot(),
                        );
                    }
                    stalled = now_stalled;
                }
            });
        probe
    }
    pub fn enter(&self, operation: u8) {
        self.operation.store(operation as u64, Ordering::Relaxed);
        self.heartbeat
            .store(self.start.elapsed().as_millis() as u64, Ordering::Release);
    }
    pub fn processed(&self, duration: Duration) {
        self.max_processing_us
            .fetch_max(duration.as_micros() as u64, Ordering::Relaxed);
    }
    fn age(&self) -> u64 {
        (self.start.elapsed().as_millis() as u64)
            .saturating_sub(self.heartbeat.load(Ordering::Acquire))
    }
    pub fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({"heartbeat_age_ms":self.age(),"operation":name(self.operation.load(Ordering::Relaxed)),"alive":self.alive.load(Ordering::Acquire),"active":self.active.load(Ordering::Acquire),"packet_drops":self.packet_drops.load(Ordering::Relaxed),"live_device_sets":lifecycle::live(),"worker_failures":self.restarts.load(Ordering::Relaxed),"max_processing_us":self.max_processing_us.load(Ordering::Relaxed)})
    }
    pub fn exited(&self) {
        self.alive.store(false, Ordering::Release);
        diagnostics::event("worker_exited", self.snapshot());
    }
}
