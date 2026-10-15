//! Decoder load, independent of presentation scheduling and recovery requests.
//! A hidden compositor, a 30 Hz display or one PLI is not bandwidth congestion.
#[derive(Clone, Copy)]
pub struct DecoderSample {
    pub at_ms: f64,
    pub received: u64,
    pub decoded: u64,
    pub decode_seconds: f64,
}
#[derive(Default)]
pub struct DecoderPressure {
    previous: Option<DecoderSample>,
    bad_seconds: u8,
}
impl DecoderPressure {
    pub fn sample(&mut self, current: DecoderSample) -> bool {
        let Some(previous) = self.previous else {
            self.previous = Some(current);
            return false;
        };
        let elapsed = (current.at_ms - previous.at_ms) / 1000.0;
        if elapsed < 1.0 {
            return self.bad_seconds >= 3;
        }
        self.previous = Some(current);
        let received = current.received.saturating_sub(previous.received);
        let decoded = current.decoded.saturating_sub(previous.decoded);
        let work = (current.decode_seconds - previous.decode_seconds).max(0.0);
        let overloaded = elapsed.is_finite()
            && work.is_finite()
            && elapsed <= 3.0
            && received > 0
            && (decoded == 0
                || work / elapsed >= 0.8
                // Hardware decoders can miss throughput without spending much
                // CPU time in decode. Count sustained missing decoded output,
                // never the separate HTML video presentation-drop counter.
                || (decoded as f64 / received as f64) < 0.8);
        self.bad_seconds = if overloaded {
            self.bad_seconds.saturating_add(1)
        } else {
            0
        };
        self.bad_seconds >= 3
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn sample(second: u32, decoded: u64, work: f64) -> DecoderSample {
        DecoderSample {
            at_ms: f64::from(second) * 1000.0,
            received: u64::from(second) * 60,
            decoded,
            decode_seconds: work,
        }
    }
    #[test]
    fn fast_4k_decoder_is_healthy_even_if_presenter_discards_frames() {
        let mut feedback = DecoderPressure::default();
        for second in 0..120 {
            // ~1 ms per 4K frame. Presentation counters and PLI events must not
            // turn this healthy decoder into a falling bitrate/FPS feedback loop.
            assert!(!feedback.sample(sample(
                second,
                u64::from(second) * 60,
                f64::from(second) * 0.06
            )));
        }
    }
    #[test]
    fn sustained_decoder_overload_reduces_quality_but_spikes_and_static_input_do_not() {
        let mut feedback = DecoderPressure::default();
        assert!(!feedback.sample(sample(0, 0, 0.0)));
        assert!(!feedback.sample(sample(1, 40, 0.9)));
        assert!(!feedback.sample(sample(2, 80, 1.8)));
        assert!(feedback.sample(sample(3, 120, 2.7)));
        assert!(!feedback.sample(sample(4, 180, 2.8)));
        assert!(!feedback.sample(sample(5, 180, 2.8)));
        assert!(!feedback.sample(sample(6, 180, 2.8)));
        assert!(feedback.sample(sample(7, 180, 2.8)));
        let mut static_frame = sample(8, 180, 2.8);
        static_frame.received = 7 * 60;
        assert!(!feedback.sample(static_frame));
    }
    #[test]
    fn hardware_decode_throughput_loss_is_detected_even_with_low_cpu_time() {
        let mut feedback = DecoderPressure::default();
        for second in 0..3 {
            assert!(!feedback.sample(sample(
                second,
                u64::from(second) * 30,
                f64::from(second) * 0.03
            )));
        }
        assert!(feedback.sample(sample(3, 90, 0.09)));
        assert!(!feedback.sample(sample(4, 150, 0.12)));
    }
}
