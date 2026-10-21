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
    smoothed: Option<(f64, f64)>,
    bad: u8,
    healthy: u8,
    pressured: bool,
}
impl DecoderPressure {
    pub fn sample(&mut self, current: DecoderSample) -> bool {
        if !current.at_ms.is_finite() || !current.decode_seconds.is_finite() {
            *self = Self::default();
            return false;
        }
        let Some(previous) = self.previous else {
            self.previous = Some(current);
            return false;
        };
        let elapsed = (current.at_ms - previous.at_ms) / 1000.0;
        if elapsed < 0.0
            || elapsed > 3.0
            || current.received < previous.received
            || current.decoded < previous.decoded
            || current.decode_seconds < previous.decode_seconds
        {
            // A suspended stats loop or replacement decoder cannot carry old
            // pressure into the next stream generation.
            *self = Self {
                previous: Some(current),
                ..Self::default()
            };
            return false;
        }
        if elapsed < 1.0 {
            return self.pressured;
        }
        self.previous = Some(current);
        let received = current.received - previous.received;
        let decoded = current.decoded - previous.decoded;
        if received == 0 {
            *self = Self {
                previous: Some(current),
                ..Self::default()
            };
            return false; // Static input is not decoder overload.
        }
        let work = ((current.decode_seconds - previous.decode_seconds) / elapsed).clamp(0.0, 1.0);
        let completion = (decoded as f64 / received as f64).min(1.0);
        let (work, completion) =
            self.smoothed
                .map_or((work, completion), |(old_work, old_completion)| {
                    ((old_work + work) / 2.0, (old_completion + completion) / 2.0)
                });
        self.smoothed = Some((work, completion));
        let overloaded = decoded == 0 || work >= 0.8 || completion < 0.8;
        self.bad = ((self.bad << 1) | u8::from(overloaded)) & 0b1111;
        self.healthy = if work < 0.65 && completion >= 0.9 && decoded > 0 {
            self.healthy.saturating_add(1)
        } else {
            0
        };
        if self.bad.count_ones() >= 3 {
            self.pressured = true;
        }
        if self.healthy >= 2 {
            self.pressured = false;
            self.bad = 0;
        }
        self.pressured
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Decoder {
        pressure: DecoderPressure,
        counters: DecoderSample,
    }
    impl Decoder {
        fn new() -> Self {
            let counters = DecoderSample {
                at_ms: 0.0,
                received: 0,
                decoded: 0,
                decode_seconds: 0.0,
            };
            let mut pressure = DecoderPressure::default();
            pressure.sample(counters);
            Self { pressure, counters }
        }
        fn step(&mut self, received: u64, decoded: u64, work: f64) -> bool {
            self.counters.at_ms += 1000.0;
            self.counters.received += received;
            self.counters.decoded += decoded;
            self.counters.decode_seconds += work;
            self.pressure.sample(self.counters)
        }
    }

    #[test]
    fn fast_4k_decoder_and_static_input_stay_healthy() {
        let mut decoder = Decoder::new();
        for _ in 0..120 {
            assert!(!decoder.step(60, 60, 0.06));
        }
        for _ in 0..120 {
            assert!(!decoder.step(0, 0, 0.0));
            assert!(!decoder.step(1, 1, 0.001));
        }
    }

    #[test]
    fn isolated_cpu_and_missing_output_spikes_are_smoothed() {
        let mut decoder = Decoder::new();
        for _ in 0..20 {
            assert!(!decoder.step(60, 60, 0.06));
        }
        assert!(!decoder.step(60, 0, 0.95));
        for _ in 0..20 {
            assert!(!decoder.step(60, 60, 0.06));
        }
    }

    #[test]
    fn sustained_cpu_or_hardware_throughput_pressure_has_stable_recovery() {
        for (decoded, work) in [(60, 0.95), (30, 0.03), (0, 0.0)] {
            let mut decoder = Decoder::new();
            for _ in 0..5 {
                decoder.step(60, 60, 0.06);
            }
            for _ in 0..6 {
                decoder.step(60, decoded, work);
            }
            assert!(decoder.pressure.pressured);
            // One good interval does not release an established overload.
            assert!(decoder.step(60, 60, 0.06));
            for _ in 0..6 {
                decoder.step(60, 60, 0.06);
            }
            assert!(!decoder.pressure.pressured);
        }
    }

    #[test]
    fn threshold_oscillation_does_not_chatter_pressure() {
        let mut decoder = Decoder::new();
        for _ in 0..6 {
            decoder.step(60, 60, 0.95);
        }
        assert!(decoder.pressure.pressured);
        for second in 0..60 {
            assert!(decoder.step(60, 60, if second % 2 == 0 { 0.78 } else { 0.82 }));
        }
        for _ in 0..6 {
            decoder.step(60, 60, 0.06);
        }
        assert!(!decoder.pressure.pressured);
    }

    #[test]
    fn idle_gap_counter_reset_and_invalid_stats_discard_old_pressure() {
        for reset in 0..6 {
            let mut decoder = Decoder::new();
            for _ in 0..6 {
                decoder.step(60, 0, 0.0);
            }
            assert!(decoder.pressure.pressured);
            let mut counters = decoder.counters;
            match reset {
                0 => counters.at_ms += 10_000.0,
                1 => {
                    counters.at_ms += 1000.0;
                    counters.received = 0;
                }
                2 => counters.at_ms = f64::NAN,
                3 => counters.decode_seconds = f64::INFINITY,
                4 => counters.at_ms -= 1000.0,
                _ => counters.at_ms += 1000.0, // Idle without arriving frames.
            }
            assert!(!decoder.pressure.sample(counters));
            assert!(!decoder.pressure.pressured);
        }
    }

    #[test]
    fn duplicate_subsecond_stats_do_not_accumulate_overload_evidence() {
        let mut decoder = Decoder::new();
        assert!(!decoder.step(60, 0, 0.0));
        for _ in 0..100 {
            assert!(!decoder.pressure.sample(decoder.counters));
        }
        decoder.counters.at_ms += 100.0;
        assert!(!decoder.pressure.sample(decoder.counters));
        assert!(!decoder.step(60, 0, 0.0));
        assert!(decoder.step(60, 0, 0.0));
    }
}
