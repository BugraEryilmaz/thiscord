use std::time::{Duration, Instant};

/// Runs on the decoder worker, never in a device callback. Silence/PLC must not
/// keep an indicator alive; a short hangover avoids flickering between syllables.
#[derive(Default)]
pub(super) struct Activity(Option<Instant>);
impl Activity {
    pub fn observe(&mut self, pcm: &[f32], now: Instant) {
        if !pcm.is_empty() && pcm.iter().map(|v| v * v).sum::<f32>() / pcm.len() as f32 > 0.00001 {
            self.0 = Some(now);
        }
    }
    pub fn speaking(&self, now: Instant) -> bool {
        self.0
            .is_some_and(|t| now.duration_since(t) < Duration::from_millis(250))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn silence_and_missing_packets_expire_activity() {
        let now = Instant::now();
        let mut activity = Activity::default();
        activity.observe(&[0.0; 960], now);
        assert!(!activity.speaking(now));
        activity.observe(&[0.03; 960], now);
        assert!(activity.speaking(now + Duration::from_millis(200)));
        activity.observe(&[0.0; 960], now + Duration::from_millis(220));
        assert!(!activity.speaking(now + Duration::from_millis(251)));
        assert!(!Activity::default().speaking(now));
    }
}
