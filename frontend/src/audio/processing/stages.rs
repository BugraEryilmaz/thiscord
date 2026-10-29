//! Ordered, replaceable capture stages. These run only on the native worker.
//! Each stage consumes/emits exactly 10 ms of normalized 48 kHz mono PCM.
use crate::audio::failure::Failure;
use sonora::{AudioProcessing, Config, StreamConfig, config::NoiseSuppressionLevel};
use thiscord_shared::audio::NoiseSuppressionStrength;

pub type Block = [f32; 480];

/// Maximum noise attenuation for models that mix back unprocessed signal.
/// These match Sonora's nominal Low/Moderate/High levels; Maximum is unlimited.
pub fn attenuation_limit_db(strength: NoiseSuppressionStrength) -> Option<f32> {
    match strength {
        NoiseSuppressionStrength::Light => Some(6.0),
        NoiseSuppressionStrength::Balanced => Some(12.0),
        NoiseSuppressionStrength::Strong => Some(18.0),
        NoiseSuppressionStrength::Maximum => None,
    }
}
/// Linear amplitude of unprocessed signal retained in `wet * (1 - m) + dry * m`.
pub fn dry_mix(strength: NoiseSuppressionStrength) -> f32 {
    attenuation_limit_db(strength).map_or(0.0, |db| 10f32.powf(-db / 20.0))
}
pub fn sonora_level(strength: NoiseSuppressionStrength) -> NoiseSuppressionLevel {
    match strength {
        NoiseSuppressionStrength::Light => NoiseSuppressionLevel::Low,
        NoiseSuppressionStrength::Balanced => NoiseSuppressionLevel::Moderate,
        NoiseSuppressionStrength::Strong => NoiseSuppressionLevel::High,
        NoiseSuppressionStrength::Maximum => NoiseSuppressionLevel::VeryHigh,
    }
}

pub trait CaptureStage {
    fn name(&self) -> &'static str;
    fn process(&mut self, block: &mut Block) -> Result<(), Failure>;
    /// Restore fresh state, including delayed samples. Never reload model files here.
    fn reset(&mut self);
    /// Explicit block buffering; frequency-dependent filter-bank delay is excluded.
    fn delay_samples(&self) -> usize {
        0
    }
    /// Denoisers apply this live without reloading models; others ignore it.
    fn set_strength(&mut self, _strength: NoiseSuppressionStrength) {}
}

/// Stages execute in insertion order. Bypassed stages consume no inference time.
/// The caller builds the chain before opening devices; no mutable graph over IPC.
#[derive(Default)]
pub struct CaptureChain {
    stages: Vec<(bool, Box<dyn CaptureStage>)>,
}
impl CaptureChain {
    pub(super) fn replace(
        &mut self,
        index: usize,
        stage: Box<dyn CaptureStage>,
    ) -> Box<dyn CaptureStage> {
        std::mem::replace(&mut self.stages[index].1, stage)
    }
    pub fn push(&mut self, enabled: bool, stage: impl CaptureStage + 'static) {
        self.stages.push((enabled, Box::new(stage)));
    }
    pub(super) fn push_boxed(&mut self, enabled: bool, stage: Box<dyn CaptureStage>) {
        self.stages.push((enabled, stage));
    }
    pub(super) fn set_strength(&mut self, index: usize, strength: NoiseSuppressionStrength) {
        self.stages[index].1.set_strength(strength);
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
    pub fn process(&mut self, block: &mut Block) -> Result<(), Failure> {
        self.process_observed(block, &mut |_, _| {})
    }
    pub(super) fn process_observed(
        &mut self,
        block: &mut Block,
        observe: &mut impl FnMut(usize, &Block),
    ) -> Result<(), Failure> {
        if block.iter().any(|x| !x.is_finite()) {
            block.fill(0.0);
            return Err(Failure::new(
                "capture_chain",
                "non_finite_input",
                "Non-finite microphone samples",
            ));
        }
        for (index, (enabled, stage)) in self.stages.iter_mut().enumerate() {
            if !*enabled {
                observe(index, block);
                continue;
            }
            if let Err(mut error) = stage.process(block) {
                block.fill(0.0);
                error.message = format!("{}: {error}", stage.name());
                return Err(error);
            }
            if block.iter().any(|x| !x.is_finite()) {
                block.fill(0.0);
                return Err(Failure::new(
                    stage.name(),
                    "non_finite_output",
                    format!("{} produced invalid audio", stage.name()),
                ));
            }
            observe(index, block);
        }
        Ok(())
    }
}

/// Stateless, bounded boost on the worker; never runs in a device callback.
pub(super) struct FixedGain(pub f32);
impl CaptureStage for FixedGain {
    fn name(&self) -> &'static str {
        "Microphone boost"
    }
    fn process(&mut self, block: &mut Block) -> Result<(), Failure> {
        if self.0 == 1.0 {
            return Ok(());
        }
        for sample in block {
            *sample = (*sample * self.0).clamp(-1.0, 1.0);
        }
        Ok(())
    }
    fn reset(&mut self) {}
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
    fn process(&mut self, block: &mut Block) -> Result<(), Failure> {
        let mut out = [0.0; 480];
        self.processor
            .process_capture_f32(&[block], &mut [&mut out])
            .map_err(|e| Failure::sonora(self.name, e))?;
        *block = out;
        Ok(())
    }
    fn reset(&mut self) {
        *self = Self::new(self.name, self.config.clone());
    }
    fn set_strength(&mut self, strength: NoiseSuppressionStrength) {
        if let Some(suppression) = &mut self.config.noise_suppression
            && suppression.level != sonora_level(strength)
        {
            suppression.level = sonora_level(strength);
            self.reset();
        }
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
        fn process(&mut self, b: &mut Block) -> Result<(), Failure> {
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
        let error = c.process(&mut b).unwrap_err();
        assert_eq!(error.details()["stage"], "test");
        assert_eq!(error.details()["code"], "non_finite_output");
        assert_eq!(b, [0.0; 480]);
    }
    #[test]
    fn failing_stage_retains_its_code_without_recording_message_contents() {
        struct Failing;
        impl CaptureStage for Failing {
            fn name(&self) -> &'static str {
                "test_inference"
            }
            fn reset(&mut self) {}
            fn process(&mut self, _: &mut Block) -> Result<(), Failure> {
                Err(Failure::new(
                    "test_inference",
                    "inference_failed",
                    "private path",
                ))
            }
        }
        let mut chain = CaptureChain::default();
        chain.push(true, Failing);
        chain.push(true, Math(0.0, 0.5));
        let mut block = [0.1; 480];
        let error = chain.process(&mut block).unwrap_err();
        assert_eq!(block, [0.0; 480]);
        assert_eq!(error.details()["code"], "inference_failed");
        assert!(!error.details().to_string().contains("private path"));
    }
}
