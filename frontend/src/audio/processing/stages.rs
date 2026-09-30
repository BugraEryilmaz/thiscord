//! Ordered, replaceable capture stages. These run only on the native worker.
//! Each stage consumes/emits exactly 10 ms of normalized 48 kHz mono PCM.
use sonora::{AudioProcessing, Config, StreamConfig};

pub type Block = [f32; 480];

pub trait CaptureStage {
    fn name(&self) -> &'static str;
    fn process(&mut self, block: &mut Block) -> Result<(), String>;
    /// Restore fresh state, including delayed samples. Never reload model files here.
    fn reset(&mut self);
    /// Explicit block buffering; frequency-dependent filter-bank delay is excluded.
    fn delay_samples(&self) -> usize {
        0
    }
}

/// Stages execute in insertion order. Bypassed stages consume no inference time.
/// The caller builds the chain before opening devices; no mutable graph over IPC.
#[derive(Default)]
pub struct CaptureChain {
    stages: Vec<(bool, Box<dyn CaptureStage>)>,
}
impl CaptureChain {
    pub fn push(&mut self, enabled: bool, stage: impl CaptureStage + 'static) {
        self.stages.push((enabled, Box::new(stage)));
    }
    pub fn set_enabled(&mut self, index: usize, enabled: bool) {
        if self.stages[index].0 != enabled {
            self.stages[index].0 = enabled;
            // No old speech or gain history when changing pipeline latency.
            self.reset();
        }
    }
    pub fn reset(&mut self) {
        for (_, stage) in &mut self.stages {
            stage.reset();
        }
    }
    pub fn delay_samples(&self) -> usize {
        self.stages
            .iter()
            .filter(|(on, _)| *on)
            .map(|(_, s)| s.delay_samples())
            .sum()
    }
    pub fn process(&mut self, block: &mut Block) -> Result<(), String> {
        if block.iter().any(|x| !x.is_finite()) {
            block.fill(0.0);
            return Err("Non-finite microphone samples".into());
        }
        for (enabled, stage) in &mut self.stages {
            if !*enabled {
                continue;
            }
            if let Err(error) = stage.process(block) {
                block.fill(0.0);
                return Err(format!("{}: {error}", stage.name()));
            }
            if block.iter().any(|x| !x.is_finite()) {
                block.fill(0.0);
                return Err(format!("{} produced invalid audio", stage.name()));
            }
        }
        Ok(())
    }
}

pub(super) struct SonoraStage {
    name: &'static str,
    config: Config,
    processor: AudioProcessing,
}
impl SonoraStage {
    pub fn new(name: &'static str, config: Config) -> Self {
        let processor = AudioProcessing::builder()
            .config(config.clone())
            .capture_config(StreamConfig::new(48_000, 1))
            .build();
        Self {
            name,
            config,
            processor,
        }
    }
}
impl CaptureStage for SonoraStage {
    fn name(&self) -> &'static str {
        self.name
    }
    fn process(&mut self, block: &mut Block) -> Result<(), String> {
        let mut out = [0.0; 480];
        self.processor
            .process_capture_f32(&[block], &mut [&mut out])
            .map_err(|e| e.to_string())?;
        *block = out;
        Ok(())
    }
    fn reset(&mut self) {
        *self = Self::new(self.name, self.config.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Math(f32, f32);
    impl CaptureStage for Math {
        fn name(&self) -> &'static str {
            "test"
        }
        fn process(&mut self, b: &mut Block) -> Result<(), String> {
            b.iter_mut().for_each(|v| *v = *v * self.0 + self.1);
            Ok(())
        }
        fn reset(&mut self) {}
    }
    #[test]
    fn ordered_composition_and_independent_bypass() {
        let mut c = CaptureChain::default();
        c.push(true, Math(2.0, 0.0));
        c.push(true, Math(1.0, 0.1));
        let mut b = [0.1; 480];
        c.process(&mut b).unwrap();
        assert_eq!(b, [0.3; 480]);
        c.set_enabled(0, false);
        b.fill(0.1);
        c.process(&mut b).unwrap();
        assert_eq!(b, [0.2; 480]);
        c.set_enabled(1, false);
        c.process(&mut b).unwrap();
        assert_eq!(b, [0.2; 480]);
    }
    #[test]
    fn invalid_stage_output_stops_before_downstream_and_silences_block() {
        let mut c = CaptureChain::default();
        c.push(true, Math(f32::NAN, 0.0));
        c.push(true, Math(0.0, 0.5));
        let mut b = [0.1; 480];
        assert!(c.process(&mut b).is_err());
        assert_eq!(b, [0.0; 480]);
    }
}
