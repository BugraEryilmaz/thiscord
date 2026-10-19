use super::failure::Failure;
#[cfg(test)]
use super::fixture;
use super::frames;
use sonora::{
    AudioProcessing, Config, StreamConfig,
    config::{
        AdaptiveDigital, EchoCanceller, GainController2, NoiseSuppression, NoiseSuppressionLevel,
    },
};
use std::time::Instant;
use thiscord_shared::audio::{AudioSettings, EchoDiagnostics, NoiseSuppressionModel};
#[cfg(feature = "deep-filter")]
pub mod deep_filter;
mod denormals;
#[cfg(test)]
mod double_talk;
#[cfg(feature = "neural-echo")]
pub mod neural_echo;
#[cfg(all(test, feature = "neural-echo"))]
mod neural_echo_tests;
#[cfg(all(test, feature = "deep-filter"))]
mod neural_tests;
pub mod stages;

#[derive(Default, serde::Serialize)]
pub(super) struct ResetCauses {
    capture_gap: u64,
    reference_gap: u64,
    reconfiguration: u64,
}

pub struct Processing {
    pub metrics: std::sync::Arc<super::quality::Metrics>,
    apm: AudioProcessing,
    config: Config,
    render_at: Option<Instant>,
    pub resets: u64,
    pub(super) reset_causes: ResetCauses,
    reference_level: f32,
    capture_frames: u64,
    clipped_samples: usize,
    clipped_input_percent: f32,
    chain: stages::CaptureChain,
    model: NoiseSuppressionModel,
    spare_noise: Option<Box<dyn stages::CaptureStage>>,
    automatic_gain: bool,
    input_gain: f32,
    capture_after: Option<Instant>,
    neural_requested: bool,
    neural_model_path: Option<String>,
    #[cfg(feature = "neural-echo")]
    neural_model: Option<neural_echo::model::Model>,
    #[cfg(feature = "neural-echo")]
    neural_failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
}
/// Only movable echo state crosses the preparation thread. DeepFilterNet's
/// thread-local tract runtime is cached on the processing worker instead.
pub(super) struct Prepared {
    settings: AudioSettings,
    apm: AudioProcessing,
    #[cfg(feature = "neural-echo")]
    model: Option<neural_echo::model::Model>,
}
impl Prepared {
    pub fn new(s: &AudioSettings) -> Result<Self, String> {
        s.validate()?;
        #[cfg(not(feature = "deep-filter"))]
        if s.noise_suppression_model == NoiseSuppressionModel::DeepFilterNet3 {
            return Err("This build does not include DeepFilterNet3".into());
        }
        #[cfg(not(feature = "neural-echo"))]
        if s.neural_echo {
            return Err("This build does not include the neural echo estimator".into());
        }
        Ok(Self {
            settings: s.clone(),
            apm: AudioProcessing::builder()
                .config(config(s))
                .capture_config(StreamConfig::new(48_000, 1))
                .render_config(StreamConfig::new(48_000, 1))
                .build(),
            #[cfg(feature = "neural-echo")]
            model: if s.neural_echo {
                Some(match s.neural_echo_model.as_deref() {
                    Some(path) => neural_echo::model::Model::load(std::path::Path::new(path))?,
                    None => neural_echo::model::Model::bundled()?,
                })
            } else {
                None
            },
        })
    }
}
fn config(s: &AudioSettings) -> Config {
    Config {
        echo_canceller: s.echo_cancellation.then(EchoCanceller::default),
        ..Default::default()
    }
}
fn noise_config() -> Config {
    Config {
        noise_suppression: Some(NoiseSuppression {
            level: NoiseSuppressionLevel::High,
            ..Default::default()
        }),
        ..Default::default()
    }
}
fn gain_config() -> Config {
    Config {
        // The library default only enables a limiter, not adaptive gain.
        // Start at unity and bound amplification of residual room noise.
        gain_controller2: Some(GainController2 {
            adaptive_digital: Some(AdaptiveDigital {
                initial_gain_db: 0.0,
                max_gain_db: 20.0,
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    }
}
impl Processing {
    pub fn new(s: &AudioSettings) -> Result<Self, String> {
        s.validate()?;
        let mut chain = stages::CaptureChain::default();
        match s.noise_suppression_model {
            NoiseSuppressionModel::Sonora => chain.push(
                s.noise_suppression,
                stages::SonoraStage::new("Sonora noise suppression", noise_config()),
            ),
            NoiseSuppressionModel::DeepFilterNet3 => {
                #[cfg(feature = "deep-filter")]
                chain.push(s.noise_suppression, deep_filter::DeepFilter::new()?);
                #[cfg(not(feature = "deep-filter"))]
                return Err("This build does not include DeepFilterNet3".into());
            }
        }
        chain.push(true, stages::FixedGain(s.input_gain));
        chain.push(
            s.automatic_gain,
            stages::SonoraStage::new("Automatic gain", gain_config()),
        );
        let mut processing = Self::with_chain(
            config(s),
            chain,
            s.noise_suppression_model,
            s.automatic_gain,
            s.input_gain,
        );
        processing.neural_requested = s.neural_echo;
        processing.neural_model_path = s.neural_echo_model.clone();
        if s.neural_echo {
            #[cfg(feature = "neural-echo")]
            {
                processing.neural_model = Some(match s.neural_echo_model.as_deref() {
                    Some(path) => neural_echo::model::Model::load(std::path::Path::new(path))?,
                    None => neural_echo::model::Model::bundled()?,
                });
                processing.attach_neural();
            }
            #[cfg(not(feature = "neural-echo"))]
            return Err("This build does not include the neural echo estimator".into());
        }
        Ok(processing)
    }
    fn with_chain(
        config: Config,
        chain: stages::CaptureChain,
        model: NoiseSuppressionModel,
        automatic_gain: bool,
        input_gain: f32,
    ) -> Self {
        Self {
            apm: AudioProcessing::builder()
                .config(config.clone())
                .capture_config(StreamConfig::new(48_000, 1))
                .render_config(StreamConfig::new(48_000, 1))
                .build(),
            config,
            metrics: Default::default(),
            render_at: None,
            resets: 0,
            reset_causes: ResetCauses::default(),
            reference_level: 0.0,
            capture_frames: 0,
            clipped_samples: 0,
            clipped_input_percent: 0.0,
            chain,
            model,
            spare_noise: None,
            automatic_gain,
            input_gain,
            capture_after: None,
            neural_requested: false,
            neural_model_path: None,
            #[cfg(feature = "neural-echo")]
            neural_model: None,
            #[cfg(feature = "neural-echo")]
            neural_failed: Default::default(),
        }
    }
    pub fn reset(&mut self) {
        self.apm = AudioProcessing::builder()
            .config(self.config.clone())
            .capture_config(StreamConfig::new(48_000, 1))
            .render_config(StreamConfig::new(48_000, 1))
            .build();
        #[cfg(feature = "neural-echo")]
        self.attach_neural();
        self.chain.reset();
        self.render_at = None;
        self.reference_level = 0.0;
        self.capture_frames = 0;
        self.clipped_samples = 0;
        self.clipped_input_percent = 0.0;
        self.resets += 1;
    }
    /// Discard enhancement history at a privacy boundary without restarting AEC.
    /// Old device blocks still advance AEC/reference timing, but feed zeros into
    /// the downstream chain so delayed denoiser output cannot retain them.
    pub(super) fn restrict_capture(&mut self, after: Instant) {
        self.capture_after = Some(after);
        self.clear_enhancement_history();
    }
    /// Replay and live privacy boundaries clear delayed enhancement, not AEC.
    pub fn clear_enhancement_history(&mut self) {
        self.chain.reset();
    }
    pub(super) fn needs_replacement(&self, s: &AudioSettings) -> bool {
        s.neural_echo != self.neural_requested
            || s.neural_echo_model != self.neural_model_path
            || s.noise_suppression_model != self.model
    }
    /// Called before live device playback, so selecting either denoiser later
    /// never initializes tract on the media-processing path.
    pub(super) fn prewarm(&mut self) -> Result<(), String> {
        if self.spare_noise.is_none() {
            self.spare_noise = match self.model {
                NoiseSuppressionModel::Sonora => {
                    #[cfg(feature = "deep-filter")]
                    {
                        Some(Box::new(deep_filter::DeepFilter::new()?))
                    }
                    #[cfg(not(feature = "deep-filter"))]
                    {
                        None
                    }
                }
                NoiseSuppressionModel::DeepFilterNet3 => Some(Box::new(stages::SonoraStage::new(
                    "Sonora noise suppression",
                    noise_config(),
                ))),
            };
        }
        Ok(())
    }
    pub(super) fn install(&mut self, mut prepared: Prepared) -> Result<Prepared, String> {
        let s = &prepared.settings;
        if s.noise_suppression_model != self.model {
            self.prewarm()?;
            let mut next = self
                .spare_noise
                .take()
                .ok_or("This build does not include DeepFilterNet3")?;
            next.reset();
            self.spare_noise = Some(self.chain.replace(0, next));
            self.model = s.noise_suppression_model;
        }
        self.chain.reset();
        self.chain.set_enabled(0, s.noise_suppression);
        self.set_input_gain(s.input_gain);
        self.chain.set_enabled(2, s.automatic_gain);
        self.automatic_gain = s.automatic_gain;
        self.config = config(s);
        self.neural_requested = s.neural_echo;
        self.neural_model_path = s.neural_echo_model.clone();
        std::mem::swap(&mut self.apm, &mut prepared.apm);
        #[cfg(feature = "neural-echo")]
        {
            std::mem::swap(&mut self.neural_model, &mut prepared.model);
            self.neural_failed
                .store(false, std::sync::atomic::Ordering::Relaxed);
            self.attach_neural();
        }
        self.render_at = None;
        self.reference_level = 0.0;
        self.capture_frames = 0;
        self.clipped_samples = 0;
        self.clipped_input_percent = 0.0;
        self.resets += 1;
        self.reset_causes.reconfiguration += 1;
        Ok(prepared)
    }
    pub fn settings(&mut self, s: &AudioSettings) -> Result<(), String> {
        s.validate()?;
        if self.needs_replacement(s) {
            // Offline replay may rebuild synchronously. Live audio prepares this
            // replacement on a separate thread before calling into the session.
            self.install(Prepared::new(s)?)?;
            return Ok(());
        }
        self.chain.set_enabled(0, s.noise_suppression);
        self.set_input_gain(s.input_gain);
        self.chain.set_enabled(2, s.automatic_gain);
        self.automatic_gain = s.automatic_gain;
        let config = config(s);
        if config.echo_canceller != self.config.echo_canceller
            || config.noise_suppression != self.config.noise_suppression
            || config.gain_controller2 != self.config.gain_controller2
        {
            if config.echo_canceller != self.config.echo_canceller {
                self.capture_frames = 0;
                self.clipped_samples = 0;
                self.clipped_input_percent = 0.0;
            }
            self.apm.apply_config(config.clone());
            self.config = config;
            #[cfg(feature = "neural-echo")]
            self.attach_neural();
        }
        Ok(())
    }
    fn set_input_gain(&mut self, gain: f32) {
        if gain != self.input_gain {
            self.chain.replace(1, Box::new(stages::FixedGain(gain)));
            self.input_gain = gain;
        }
    }
    /// Explicit block buffering in active stages. Excludes filter-bank phase/
    /// group delay, AEC, devices, Opus and network latency.
    pub fn enhancement_delay_samples(&self) -> usize {
        self.chain.delay_samples()
    }
    pub fn render(&mut self, frame: &[f32; 480]) -> Result<(), Failure> {
        let metrics = self.metrics.clone();
        let _timer = metrics.timed(super::quality::Stage::Render);
        let _float_mode = denormals::Guard::new();
        let peak = frame.iter().fold(0.0_f32, |peak, v| peak.max(v.abs()));
        // Hold short peaks for the UI poll; silence makes old metrics unavailable.
        self.reference_level = peak.max(self.reference_level * 0.95);
        let mut out = [0.0; 480];
        self.apm
            .process_render_f32(&[frame], &mut [&mut out])
            .map_err(|e| Failure::sonora("echo_render", e))
    }
    #[cfg(test)]
    pub(super) fn render_queued(&mut self, queue: &mut frames::Reader) -> Result<(), Failure> {
        self.render_tapped(queue, &mut |_, _| {})
    }
    pub(super) fn render_tapped(
        &mut self,
        queue: &mut frames::Reader,
        tap: &mut impl FnMut(&frames::Frame, bool),
    ) -> Result<(), Failure> {
        for _ in 0..frames::BLOCKS {
            let Some((frame, gap)) = queue.pop() else {
                break;
            };
            if gap {
                self.reset_causes.reference_gap += 1;
                self.reset();
            }
            tap(&frame, gap);
            self.render(&frame.samples)?;
            self.render_at = Some(frame.at);
        }
        Ok(())
    }
    #[cfg(test)]
    pub(super) fn capture_queued(
        &mut self,
        queue: &mut frames::Reader,
    ) -> Result<Option<[f32; 960]>, Failure> {
        Ok(self
            .capture_tapped(queue, &mut |_, _, _, _| {})?
            .map(|p| p.pcm))
    }
    pub(super) fn capture_tapped(
        &mut self,
        queue: &mut frames::Reader,
        tap: &mut impl FnMut(&frames::Frame, bool, bool, i32),
    ) -> Result<Option<Captured>, Failure> {
        if queue.len() < 2 {
            return Ok(None);
        }
        let (first, first_gap) = queue.pop().expect("two complete frames");
        let (second, second_gap) = queue.pop().expect("two complete frames");
        if second_gap {
            // An Opus packet cannot describe a hole inside its 20 ms duration.
            // Discard the earlier half and retain the later half for the next tick.
            queue.retain(second);
            return Ok(None);
        }
        if first_gap {
            self.reset_causes.capture_gap += 1;
            self.reset();
        }
        tap(
            &first,
            first_gap,
            first_gap,
            frames::delay_ms(self.render_at, first.at),
        );
        tap(
            &second,
            second_gap,
            false,
            frames::delay_ms(self.render_at, second.at),
        );
        let mut pcm = [0.0; 960];
        let mut evidence = super::gate::Evidence::default();
        pcm[..480].copy_from_slice(&first.samples);
        pcm[480..].copy_from_slice(&second.samples);
        for (index, (frame, at)) in pcm
            .as_chunks_mut::<480>()
            .0
            .iter_mut()
            .zip([first.at, second.at])
            .enumerate()
        {
            evidence.0[index] = self.capture_frame_metered(
                frame,
                frames::delay_ms(self.render_at, at),
                self.capture_after.is_none_or(|after| at >= after),
            )?;
        }
        Ok(Some(Captured {
            pcm,
            evidence,
            at: [first.at, second.at],
            gap: [first_gap, second_gap],
            position: first.position,
        }))
    }
    pub fn capture_frame(&mut self, frame: &mut [f32; 480], delay_ms: i32) -> Result<(), Failure> {
        self.capture_frame_retained(frame, delay_ms, true)
    }
    /// Advance AEC normally, but exclude pre-boundary capture from enhancement.
    /// Used by offline replay with the recorded capture-boundary timestamps.
    pub fn capture_frame_retained(
        &mut self,
        frame: &mut [f32; 480],
        delay_ms: i32,
        retain: bool,
    ) -> Result<(), Failure> {
        self.capture_frame_metered(frame, delay_ms, retain)
            .map(|_| ())
    }
    fn capture_frame_metered(
        &mut self,
        frame: &mut [f32; 480],
        delay_ms: i32,
        retain: bool,
    ) -> Result<f32, Failure> {
        let metrics = self.metrics.clone();
        let _timer = metrics.timed(super::quality::Stage::Capture);
        let _float_mode = denormals::Guard::new();
        if frame.iter().any(|x| !x.is_finite()) {
            frame.fill(0.0);
            return Err(Failure::new(
                "capture",
                "non_finite_input",
                "Non-finite microphone samples",
            ));
        }
        self.clipped_samples += frame.iter().filter(|v| v.abs() >= 0.999).count();
        self.capture_frames += 1;
        if self.capture_frames.is_multiple_of(100) {
            self.clipped_input_percent = self.clipped_samples as f32 / 480.0;
            self.clipped_samples = 0;
        }
        // Device timestamps account for buffering; AEC3 estimates the acoustic
        // path, including room reflections, from the actual playback reference.
        self.apm
            .set_stream_delay_ms(delay_ms.clamp(0, 500))
            .map_err(|e| Failure::sonora("echo_delay", e))?;
        let mut out = [0.0; 480];
        self.apm
            .process_capture_f32(&[frame], &mut [&mut out])
            .map_err(|e| Failure::sonora("echo_capture", e))?;
        #[cfg(feature = "neural-echo")]
        if self
            .neural_failed
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            frame.fill(0.0);
            return Err(Failure::new(
                "neural_echo",
                "estimator_failed",
                "Neural echo processing failed. Stop audio and select conventional echo cancellation or reload the model.",
            ));
        }
        frame.copy_from_slice(&out);
        if !retain {
            frame.fill(0.0);
        }
        let mut rms = 0.0;
        self.chain.process_observed(frame, &mut |index, block| {
            if index == 1 {
                rms = super::gate::Evidence::rms(block);
            }
        })?;
        for sample in frame.iter_mut() {
            *sample = sample.clamp(-1.0, 1.0);
        }
        Ok(rms)
    }
    pub fn echo_diagnostics(&self) -> Option<EchoDiagnostics> {
        self.config.echo_canceller.as_ref()?;
        let stats = self.apm.statistics();
        let active = self.capture_frames >= 100 && self.reference_level > 0.0001;
        Some(EchoDiagnostics {
            reference_level: self.reference_level,
            filter_reduction_db: active
                .then_some(stats.echo_return_loss_enhancement)
                .flatten()
                .filter(|v| v.is_finite())
                .map(|v| v as f32),
            estimated_delay_ms: active.then_some(stats.delay_ms).flatten(),
            clipped_input_percent: self.clipped_input_percent,
            automatic_gain: self.automatic_gain,
        })
    }
    #[cfg(feature = "neural-echo")]
    fn attach_neural(&mut self) {
        if self.config.echo_canceller.is_some()
            && let Some(model) = &self.neural_model
        {
            let estimator = neural_echo::NeuralEcho::new(model.clone(), self.neural_failed.clone());
            if !self.apm.set_neural_estimator(Box::new(estimator)) {
                self.neural_failed
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    #[cfg(test)]
    pub fn capture(&mut self, pcm: &mut [f32; 960], delay_ms: i32) -> Result<(), String> {
        for (index, frame) in pcm.as_chunks_mut::<480>().0.iter_mut().enumerate() {
            self.capture_frame(frame, delay_ms - index as i32 * 10)?;
        }
        Ok(())
    }
}
pub(super) struct Captured {
    pub pcm: [f32; 960],
    pub evidence: super::gate::Evidence,
    pub at: [Instant; 2],
    pub gap: [bool; 2],
    pub position: u64,
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn microphone_boost_opens_and_holds_quiet_speech_without_bypassing_restrictions() {
        use super::super::gate::{Evidence, Gate};
        use thiscord_shared::audio::TransmitMode;

        let start = Instant::now();
        for gain in [1.0, 4.0] {
            let settings = AudioSettings {
                input_gain: gain,
                ..Default::default()
            };
            let mut processing = Processing::new(&settings).unwrap();
            let mut gate = Gate::default();
            for packet in 0..40 {
                // A quiet onset followed by even quieter sustained speech.
                let amplitude = if packet < 10 { 0.006 } else { 0.0025 };
                let mut pcm = [0.0; 960];
                let mut evidence = Evidence::default();
                for (half, block) in pcm.as_chunks_mut::<480>().0.iter_mut().enumerate() {
                    *block = std::array::from_fn(|i| {
                        (i as f32 * 200.0 * std::f32::consts::TAU / 48_000.0).sin() * amplitude
                    });
                    evidence.0[half] = processing.capture_frame_metered(block, 0, true).unwrap();
                    let expected_rms = amplitude * gain * std::f32::consts::FRAC_1_SQRT_2;
                    assert!((evidence.0[half] - expected_rms).abs() < 1e-5);
                    assert!((Evidence::rms(block) - expected_rms).abs() < 1e-5);
                }
                let allowed = packet < 35;
                let output = gate.process(
                    pcm,
                    evidence,
                    start + std::time::Duration::from_millis(packet * 20),
                    false,
                    TransmitMode::VoiceActivity,
                    settings.activation_threshold,
                    allowed,
                    false,
                );
                if packet >= 5 {
                    assert_eq!(output.active, gain > 1.0 && allowed, "packet {packet}");
                    if !output.active {
                        assert_eq!(output.pcm, [0.0; 960]);
                    }
                }
            }
        }
    }

    #[test]
    fn microphone_boost_updates_live_survives_replacement_and_clamps() {
        let mut settings = AudioSettings::default();
        let mut processing = Processing::new(&settings).unwrap();
        for gain in [1.0, 4.0, 10.0, 1.0] {
            settings.input_gain = gain;
            processing.settings(&settings).unwrap();
            let mut block = [0.2; 480];
            let rms = processing
                .capture_frame_metered(&mut block, 0, true)
                .unwrap();
            let expected = (0.2 * gain).min(1.0);
            assert!(block.iter().all(|v| (*v - expected).abs() < 1e-6));
            assert!((rms - expected).abs() < 1e-5);
        }
        settings.input_gain = 3.0;
        processing
            .install(Prepared::new(&settings).unwrap())
            .unwrap();
        let mut block = [-0.5; 480];
        processing.capture_frame(&mut block, 0).unwrap();
        assert_eq!(block, [-1.0; 480]);
        // A pre-boundary block stays silent even at the configured boost.
        let mut block = [0.5; 480];
        processing
            .capture_frame_retained(&mut block, 0, false)
            .unwrap();
        assert_eq!(block, [0.0; 480]);
    }

    #[test]
    fn activation_evidence_is_after_denoising_and_independent_of_agc() {
        let speech = fixture::speech();
        let mut models = vec![NoiseSuppressionModel::Sonora];
        if cfg!(feature = "deep-filter") {
            models.push(NoiseSuppressionModel::DeepFilterNet3);
        }
        for model in models {
            for neural in [false, true] {
                if neural && !cfg!(feature = "neural-echo") {
                    continue;
                }
                let s = AudioSettings {
                    input_gain: 2.0,
                    noise_suppression: true,
                    noise_suppression_model: model,
                    echo_cancellation: true,
                    neural_echo: neural,
                    ..Default::default()
                };
                let mut plain = Processing::new(&s).unwrap();
                let mut gain = Processing::new(&AudioSettings {
                    automatic_gain: true,
                    ..s
                })
                .unwrap();
                let mut different = false;
                for n in 0..400 {
                    let input = std::array::from_fn(|i| speech[(n * 480 + i) % speech.len()] * 0.2);
                    plain.render(&[0.0; 480]).unwrap();
                    gain.render(&[0.0; 480]).unwrap();
                    let (mut a, mut b) = (input, input);
                    let a_rms = plain.capture_frame_metered(&mut a, 20, true).unwrap();
                    let b_rms = gain.capture_frame_metered(&mut b, 20, true).unwrap();
                    assert_eq!(a_rms, b_rms, "{model:?}, neural={neural}");
                    assert!((a_rms - super::super::gate::Evidence::rms(&a)).abs() < 1e-6);
                    different |= a != b;
                }
                assert!(
                    different,
                    "Test must exercise real AGC: {model:?}, neural={neural}"
                );
            }
        }
    }

    #[test]
    fn boundary_discards_old_device_blocks_and_denoiser_history_without_resetting_aec() {
        let mut models = vec![NoiseSuppressionModel::Sonora];
        if cfg!(feature = "deep-filter") {
            models.push(NoiseSuppressionModel::DeepFilterNet3);
        }
        for model in models {
            let mut p = Processing::new(&AudioSettings {
                noise_suppression: true,
                noise_suppression_model: model,
                ..Default::default()
            })
            .unwrap();
            for _ in 0..20 {
                p.capture_frame(&mut [0.3; 480], 0).unwrap();
            }
            let at = Instant::now();
            p.restrict_capture(at + std::time::Duration::from_millis(40));
            let (mut writer, mut reader) = frames::queue();
            writer.begin(at);
            for _ in 0..1920 {
                writer.push(0.8);
            }
            for _ in 0..2880 {
                writer.push(0.0);
            }
            while let Some(packet) = p.capture_tapped(&mut reader, &mut |_, _, _, _| {}).unwrap() {
                assert!(packet.pcm.iter().all(|v| v.abs() < 1e-8), "{model:?}");
            }
            assert_eq!(p.resets, 0, "Privacy boundaries do not restart AEC");
        }
    }
    #[test]
    fn gap_inside_packet_retains_later_half_instead_of_compressing_time() {
        let (mut writer, mut reader) = frames::queue();
        let at = Instant::now();
        writer.begin(at);
        for _ in 0..(frames::BLOCKS + 1) * 480 {
            writer.push(0.1);
        }
        for _ in 0..frames::BLOCKS - 1 {
            reader.pop().unwrap();
        }
        writer.begin(at + frames::sample_duration((frames::BLOCKS + 1) * 480));
        for _ in 0..480 {
            writer.push(0.2);
        }
        let mut p = Processing::new(&AudioSettings::default()).unwrap();
        let mut taps = Vec::new();
        assert!(
            p.capture_tapped(&mut reader, &mut |_, _, _, _| {})
                .unwrap()
                .is_none()
        );
        assert_eq!(reader.len(), 1);
        for _ in 0..480 {
            writer.push(0.3);
        }
        let captured = p
            .capture_tapped(&mut reader, &mut |frame, gap, reset, delay| {
                taps.push((frame.samples[0], gap, reset, delay))
            })
            .unwrap()
            .unwrap();
        assert_eq!(captured.gap, [true, false]);
        assert_eq!(captured.position, (frames::BLOCKS as u64 + 1) * 480);
        assert_eq!(taps, [(0.2, true, true, 0), (0.3, false, false, 0)]);
        assert_eq!(&captured.pcm[..480], &[0.2; 480]);
        assert_eq!(&captured.pcm[480..], &[0.3; 480]);
        assert_eq!(p.resets, 1);
    }

    fn noise(state: &mut u32) -> f32 {
        *state = state.wrapping_mul(1664525).wrapping_add(1013904223);
        (*state as f64 / u32::MAX as f64 * 2.0 - 1.0) as f32
    }

    fn energy(samples: &[f32]) -> f64 {
        samples.iter().map(|&v| f64::from(v).powi(2)).sum()
    }

    #[test]
    fn echo_diagnostics_report_clipping_and_expire_without_a_reference() {
        let mut p = Processing::new(&AudioSettings {
            echo_cancellation: true,
            ..Default::default()
        })
        .unwrap();
        assert!(p.echo_diagnostics().unwrap().filter_reduction_db.is_none());
        for _ in 0..100 {
            p.render(&[0.05; 480]).unwrap();
            let mut input = [0.1; 480];
            input[..48].fill(1.0);
            p.capture_frame(&mut input, 60).unwrap();
        }
        let stats = p.echo_diagnostics().unwrap();
        assert_eq!(stats.clipped_input_percent, 10.0);
        assert!(stats.estimated_delay_ms.is_some());
        assert!(stats.filter_reduction_db.is_some_and(f32::is_finite));
        // A silent reference must not advertise an old estimate as current.
        for _ in 0..200 {
            p.render(&[0.0; 480]).unwrap();
        }
        assert!(p.echo_diagnostics().unwrap().filter_reduction_db.is_none());
        p.reset();
        assert_eq!(p.echo_diagnostics().unwrap().clipped_input_percent, 0.0);
        p.settings(&AudioSettings::default()).unwrap();
        assert!(p.echo_diagnostics().is_none());
    }

    #[test]
    fn stationary_noise_is_attenuated() {
        let mut p = Processing::new(&AudioSettings {
            noise_suppression: true,
            ..Default::default()
        })
        .unwrap();
        let mut state = 42;
        let (mut before, mut after) = (0.0, 0.0);
        for frame in 0..400 {
            let mut pcm = std::array::from_fn(|_| noise(&mut state) * 0.03);
            let input = energy(&pcm);
            p.capture(&mut pcm, 40).unwrap();
            if frame >= 200 {
                before += input;
                after += energy(&pcm);
            }
        }
        let reduction = 10.0 * (before / after).log10();
        eprintln!("Stationary noise reduction: {reduction:.1} dB");
        assert!(reduction > 17.0, "noise reduction: {reduction:.1} dB");
    }

    #[test]
    fn echo_cancellation_handles_batched_device_callbacks() {
        for batch in [960, 4800] {
            let mut p = Processing::new(&AudioSettings {
                echo_cancellation: true,
                ..Default::default()
            })
            .unwrap();
            let (mut render, mut references) = frames::queue();
            let (mut capture, mut captures) = frames::queue();
            let mut state = 456;
            let history: Vec<_> = (0..48_000 * 12).map(|_| noise(&mut state) * 0.2).collect();
            let start = Instant::now();
            let (mut before, mut after, mut delivered) = (0.0, 0.0, 0);
            for offset in (0..history.len()).step_by(batch) {
                let at = start + frames::sample_duration(offset);
                // Independent devices: playback is 50 ms ahead, captured audio
                // is 30 ms old. Add 5 ms propagation and a 15 ms room reflection.
                render.begin(at + std::time::Duration::from_millis(50));
                capture.begin(at - std::time::Duration::from_millis(30));
                for n in offset..offset + batch {
                    assert_eq!(render.push(history[n]), 0);
                    let sample = n.checked_sub(4080).map_or(0.0, |j| history[j] * 0.6)
                        + n.checked_sub(4800).map_or(0.0, |j| history[j] * 0.15);
                    assert_eq!(capture.push(sample), 0);
                    if offset >= 48_000 * 8 {
                        before += f64::from(sample).powi(2);
                    }
                }
                p.render_queued(&mut references).unwrap();
                while let Some(pcm) = p.capture_queued(&mut captures).unwrap() {
                    delivered += pcm.len();
                    if offset >= 48_000 * 8 {
                        after += energy(&pcm);
                    }
                }
            }
            assert_eq!(delivered, history.len());
            assert_eq!(p.resets, 0);
            let reduction = 10.0 * (before / after).log10();
            eprintln!(
                "Echo reduction, {} ms callbacks: {reduction:.1} dB",
                batch / 48
            );
            assert!(reduction > 20.0, "batch {batch}: {reduction:.1} dB");
        }
    }

    #[test]
    fn echo_cancellation_preserves_near_end_audio_without_playback() {
        let mut p = Processing::new(&AudioSettings {
            echo_cancellation: true,
            ..Default::default()
        })
        .unwrap();
        let (mut before, mut after) = (0.0, 0.0);
        for frame in 0..300 {
            // Voiced harmonics, independent of the silent speaker reference.
            let mut pcm = std::array::from_fn(|i| {
                let t = (frame * 960 + i) as f32 / 48_000.0;
                [220.0, 440.0, 880.0]
                    .into_iter()
                    .map(|hz| (t * hz * std::f32::consts::TAU).sin() * 0.03)
                    .sum()
            });
            p.render(&[0.0; 480]).unwrap();
            p.render(&[0.0; 480]).unwrap();
            let input = energy(&pcm);
            p.capture(&mut pcm, 40).unwrap();
            if frame > 200 {
                before += input;
                after += energy(&pcm);
            }
        }
        assert!(
            after / before > 0.5,
            "near-end speech must not be muted: {}",
            after / before
        );
    }

    #[test]
    fn disabling_suppression_live_restores_unprocessed_audio() {
        let mut p = Processing::new(&AudioSettings {
            noise_suppression: true,
            ..Default::default()
        })
        .unwrap();
        for _ in 0..50 {
            p.capture(&mut [0.01; 960], 0).unwrap();
        }
        p.settings(&AudioSettings::default()).unwrap();
        let mut state = 789;
        let input = std::array::from_fn(|_| noise(&mut state) * 0.1);
        let mut output = input;
        p.capture(&mut output, 0).unwrap();
        assert_eq!(input, output);
    }

    #[test]
    fn capture_and_reference_overflow_restart_adaptation_once_per_gap() {
        for render_gap in [false, true] {
            let mut p = Processing::new(&AudioSettings {
                echo_cancellation: true,
                ..Default::default()
            })
            .unwrap();
            let (mut writer, mut reader) = frames::queue();
            for _ in 0..(frames::BLOCKS + 2) * frames::SAMPLES {
                writer.push(0.0);
            }
            if render_gap {
                p.render_queued(&mut reader).unwrap();
            } else {
                while p.capture_queued(&mut reader).unwrap().is_some() {}
            }
            assert_eq!(p.resets, 0);
            for _ in 0..960 {
                writer.push(0.0);
            }
            if render_gap {
                p.render_queued(&mut reader).unwrap();
            } else {
                assert!(p.capture_queued(&mut reader).unwrap().is_some());
            }
            assert_eq!(p.resets, 1);
            assert_eq!(p.reset_causes.reference_gap, u64::from(render_gap));
            assert_eq!(p.reset_causes.capture_gap, u64::from(!render_gap));
            assert_eq!(p.reset_causes.reconfiguration, 0);
        }
    }

    #[test]
    fn delayed_speaker_echo_is_attenuated() {
        let mut p = Processing::new(&AudioSettings {
            echo_cancellation: true,
            ..Default::default()
        })
        .unwrap();
        let mut state = 123;
        let mut history = vec![0.0; 48_000 * 12];
        let (mut before, mut after) = (0.0, 0.0);
        for frame in 0..600 {
            let offset = frame * 960;
            for sample in &mut history[offset..offset + 960] {
                *sample = noise(&mut state) * 0.2;
            }
            for render in history[offset..offset + 960].as_chunks::<480>().0 {
                p.render(render).unwrap();
            }
            let mut pcm = std::array::from_fn(|i| {
                let n = offset + i;
                // A 40 ms direct path plus a quieter room reflection at 55 ms.
                n.checked_sub(1920).map_or(0.0, |j| history[j] * 0.6)
                    + n.checked_sub(2640).map_or(0.0, |j| history[j] * 0.15)
            });
            let input = energy(&pcm);
            p.capture(&mut pcm, 40).unwrap();
            if frame >= 400 {
                before += input;
                after += energy(&pcm);
            }
        }
        let reduction = 10.0 * (before / after).log10();
        eprintln!("Delayed echo reduction: {reduction:.1} dB");
        assert!(reduction > 20.0, "echo reduction: {reduction:.1} dB");
    }
    #[test]
    fn processing_stays_finite_with_render_reference() {
        let s = AudioSettings {
            noise_suppression: true,
            automatic_gain: true,
            echo_cancellation: true,
            ..Default::default()
        };
        let mut p = Processing::new(&s).unwrap();
        for _ in 0..20 {
            p.render(&[0.0; 480]).unwrap();
            let mut input = [0.001; 960];
            p.capture(&mut input, 40).unwrap();
            assert!(input.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        }
    }
}
