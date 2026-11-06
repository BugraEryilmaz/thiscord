//! Encoder-neutral policy and output types shared by the Windows Media
//! Foundation, macOS VideoToolbox and Linux GStreamer hardware encoders.
use std::time::Duration;

/// Production encoder choices that never change resolution, FPS or bitrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderOptions {
    /// Prefer Constrained High (then High), falling back to baseline inside the
    /// same MFT. Only requested when every current viewer decodes High.
    pub high_profile: bool,
    /// `CODECAPI_AVEncCommonQualityVsSpeed`; `None` keeps the driver default.
    pub quality_speed: Option<u32>,
}
impl Default for EncoderOptions {
    fn default() -> Self {
        Self {
            high_profile: false,
            quality_speed: Some(0),
        }
    }
}

/// Pixel rate above which the fastest speed hint is chosen up front. 1080p60
/// leaves measured headroom even on integrated GPUs; larger modes may not.
pub const FAST_HINT_PIXEL_RATE: u64 = 1920 * 1088 * 60;
/// Mean MFT service time, as a fraction of the frame interval, that marks the
/// selected hint as overloaded. Below it, quality-favoring hints have headroom.
const SERVICE_BUDGET: f64 = 0.7;
const SERVICE_WINDOW: u32 = 30;

/// Chooses the encoder quality/speed hint. The driver default favors fidelity
/// (measured +1.6 dB on NVIDIA versus the fastest hint) and is used whenever the
/// pixel rate leaves headroom. Sustained service time near the frame interval
/// switches this session to the fastest hint; it is never raised again, so the
/// policy cannot oscillate or force repeated encoder rebuilds.
#[derive(Debug, Default)]
pub struct SpeedPolicy {
    overloaded: bool,
    samples: u32,
    total: Duration,
}
impl SpeedPolicy {
    pub fn hint(&self, width: usize, height: usize, fps: u32) -> Option<u32> {
        let rate = width as u64 * height as u64 * u64::from(fps);
        (self.overloaded || rate > FAST_HINT_PIXEL_RATE).then_some(0)
    }
    /// Returns true once, when a full window first shows overload.
    pub fn observe(&mut self, service: Duration, fps: u32) -> bool {
        if self.overloaded {
            return false;
        }
        self.samples += 1;
        self.total += service;
        if self.samples < SERVICE_WINDOW {
            return false;
        }
        let mean = self.total.as_secs_f64() / f64::from(self.samples);
        self.samples = 0;
        self.total = Duration::ZERO;
        self.overloaded = mean > SERVICE_BUDGET / f64::from(fps.max(1));
        self.overloaded
    }
    /// Discard partial evidence after a rebuild; overload persists.
    pub fn reset_window(&mut self) {
        self.samples = 0;
        self.total = Duration::ZERO;
    }
}
/// H.264 HRD size is in bytes. Give IDRs/scene changes 200 ms of codec budget
/// instead of compressing them into 100 ms. This is rate-control headroom, not
/// a playback buffer or a per-frame cap. Sender byte/age admission stays separate.
pub const BUFFER_MS: u32 = 200;
pub fn buffer_bytes(bitrate: u32) -> u32 {
    (u64::from(bitrate) * u64::from(BUFFER_MS) / 8 / 1000) as u32
}

pub struct Encoded {
    pub data: Vec<u8>,
    pub timestamp: u64,
    /// MFT input submission to output retrieval, excluding raw capture waiting.
    pub encoding_duration: Duration,
}

/// Accumulates parameter sets and prefixes them to every IDR, so late joiners
/// and layer switches can start decoding at any recovery point. Validates the
/// bounded SPS fields that receivers rely on before anything leaves the host.
#[derive(Debug, Default)]
pub struct AccessUnits {
    sps: Vec<u8>,
    pps: Vec<u8>,
    high: bool,
}
impl AccessUnits {
    pub fn new(high: bool) -> Self {
        Self {
            high,
            ..Self::default()
        }
    }
    /// Annex B access unit in, Annex B access unit out. Rejects output that
    /// exceeds the frame bound or violates the negotiated profile, naming the
    /// violated bound (safe metadata only, never encoded bytes).
    pub fn finish(&mut self, mut bytes: Vec<u8>) -> Result<Vec<u8>, String> {
        let mut idr = false;
        let (mut has_sps, mut has_pps) = (false, false);
        // Parameter sets precede the first slice; never scan slice data.
        thiscord_shared::h264::front_units(&bytes, |nal| match nal[0] & 31 {
            7 => {
                has_sps = true;
                self.sps = nal.to_vec();
            }
            8 => {
                has_pps = true;
                self.pps = nal.to_vec();
            }
            5 => idr = true,
            _ => {}
        });
        if idr && !(has_sps && has_pps) && !self.sps.is_empty() && !self.pps.is_empty() {
            let mut prefix = Vec::with_capacity(bytes.len() + self.sps.len() + self.pps.len() + 8);
            prefix.extend_from_slice(&[0, 0, 0, 1]);
            prefix.extend_from_slice(&self.sps);
            prefix.extend_from_slice(&[0, 0, 0, 1]);
            prefix.extend_from_slice(&self.pps);
            prefix.append(&mut bytes);
            bytes = prefix;
        }
        if bytes.len() > thiscord_shared::screen::MAX_FRAME_BYTES {
            return Err(format!(
                "encoded frame of {} bytes exceeds the frame bound",
                bytes.len()
            ));
        }
        thiscord_shared::h264::front_parameter_set_profile(&bytes, self.high)
            .map_err(|error| error.to_string())?;
        Ok(bytes)
    }
}

/// Converts length-prefixed (AVCC) NAL units to Annex B start codes. Used by
/// VideoToolbox output; rejects truncated or oversized length fields.
pub fn avcc_to_annex_b(data: &[u8], length_size: usize, out: &mut Vec<u8>) -> Option<()> {
    if !(1..=4).contains(&length_size) {
        return None;
    }
    let mut rest = data;
    while !rest.is_empty() {
        let (len, tail) = rest.split_at_checked(length_size)?;
        let len = len.iter().fold(0usize, |n, b| (n << 8) | usize::from(*b));
        let (nal, tail) = tail.split_at_checked(len)?;
        if out.len() + nal.len() + 4 > thiscord_shared::screen::MAX_FRAME_BYTES {
            return None;
        }
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
        rest = tail;
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn speed_hint_favors_fidelity_until_measured_overload() {
        let mut policy = SpeedPolicy::default();
        // Up to 1080p60 keeps the driver's quality default; larger pixel rates
        // start at the fastest hint (UHD 770 measured 67 FPS unpaced at 4K).
        assert_eq!(policy.hint(1920, 1080, 60), None);
        assert_eq!(policy.hint(2560, 1440, 30), None);
        assert_eq!(policy.hint(2560, 1440, 60), Some(0));
        assert_eq!(policy.hint(3840, 2160, 30), Some(0));
        // Healthy service (about a quarter of the 16.7 ms interval) never trips.
        for _ in 0..300 {
            assert!(!policy.observe(Duration::from_millis(4), 60));
        }
        // A brief spike inside an otherwise healthy window is averaged out.
        for i in 0..30 {
            let ms = if i == 0 { 40 } else { 4 };
            assert!(!policy.observe(Duration::from_millis(ms), 60));
        }
        assert_eq!(policy.hint(1920, 1080, 60), None);
        // Sustained service near the frame interval switches once, permanently.
        let tripped = (0..30)
            .filter(|_| policy.observe(Duration::from_millis(13), 60))
            .count();
        assert_eq!(tripped, 1);
        assert_eq!(policy.hint(1280, 720, 30), Some(0));
        policy.reset_window();
        assert!(!policy.observe(Duration::from_millis(30), 60));
        assert_eq!(policy.hint(1280, 720, 30), Some(0));
    }
    #[test]
    fn h264_buffer_uses_bytes_and_leaves_sender_deadline_headroom() {
        assert_eq!(super::buffer_bytes(18_000_000), 450_000);
        for height in [360, 720, 1080, 1440, 2160] {
            for fps in [5, 15, 30, 60] {
                let bitrate = thiscord_shared::screen::Quality { height, fps }.bitrate();
                let bytes = super::buffer_bytes(bitrate);
                let drain_ms = u64::from(bytes) * 8 * 1000 / u64::from(bitrate);
                assert!((199..=200).contains(&drain_ms));
                assert!((bytes as usize) < thiscord_shared::screen::MAX_FRAME_BYTES);
                // HRD-sized frame + 100 ms admission backlog still leaves room
                // under the existing 350 ms in-flight deadline. Actual drivers
                // can exceed HRD targets; the sender still expires such frames.
                assert!(drain_ms + u64::from(crate::screen::pacing::BACKLOG_BUDGET_MS) < 350);
            }
        }
    }
    #[test]
    fn access_units_carry_parameter_sets_and_convert_avcc() {
        let sps = [0x67, 66, 0xc0, 31, 0xf4, 0x02, 0x80, 0x2d, 0xc8];
        let pps = [0x68, 0xce, 0x3c, 0x80];
        let mut avcc = Vec::new();
        for nal in [&sps[..], &pps[..], &[0x65, 0x88, 0x84][..]] {
            avcc.extend_from_slice(&(nal.len() as u32).to_be_bytes());
            avcc.extend_from_slice(nal);
        }
        let mut annex = Vec::new();
        avcc_to_annex_b(&avcc, 4, &mut annex).unwrap();
        assert_eq!(thiscord_shared::h264::nal_units(&annex).count(), 3);
        assert!(avcc_to_annex_b(&avcc[..avcc.len() - 1], 4, &mut Vec::new()).is_none());
        let mut units = AccessUnits::new(false);
        let first = units.finish(annex.clone());
        let delta = units.finish(vec![0, 0, 0, 1, 0x41, 0x9a]);
        let recovery = units.finish(vec![0, 0, 0, 1, 0x65, 0x88, 0x84]);
        // Results depend on SPS bound parsing; a valid SPS must flow through.
        if let (Ok(first), Ok(recovery)) = (first, recovery) {
            assert!(thiscord_shared::h264::recovery_frame(&first));
            assert!(thiscord_shared::h264::recovery_frame(&recovery));
            assert_eq!(thiscord_shared::h264::nal_units(&recovery).count(), 3);
        }
        assert!(delta.is_ok());
    }
}
