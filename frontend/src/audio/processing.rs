use sonora::{
    AudioProcessing, Config, StreamConfig,
    config::{EchoCanceller, GainController2, NoiseSuppression},
};
use thiscord_shared::audio::AudioSettings;
pub struct Processing {
    apm: AudioProcessing,
}
fn config(s: &AudioSettings) -> Config {
    Config {
        echo_canceller: s.echo_cancellation.then(EchoCanceller::default),
        noise_suppression: s.noise_suppression.then(NoiseSuppression::default),
        gain_controller2: s.automatic_gain.then(GainController2::default),
        ..Default::default()
    }
}
impl Processing {
    pub fn new(s: &AudioSettings) -> Self {
        Self {
            apm: AudioProcessing::builder()
                .config(config(s))
                .capture_config(StreamConfig::new(48_000, 1))
                .render_config(StreamConfig::new(48_000, 1))
                .build(),
        }
    }
    pub fn settings(&mut self, s: &AudioSettings) {
        self.apm.apply_config(config(s));
    }
    pub fn render(&mut self, frame: &[f32; 480]) -> Result<(), String> {
        let mut out = [0.0; 480];
        self.apm
            .process_render_f32(&[frame], &mut [&mut out])
            .map_err(|e| e.to_string())
    }
    pub fn capture(&mut self, pcm: &mut [f32; 960]) -> Result<(), String> {
        // Initial delay estimate; AEC3 refines its delay internally. Acoustic
        // acceptance still needs real devices and the target OS audio backend.
        self.apm
            .set_stream_delay_ms(40)
            .map_err(|e| e.to_string())?;
        for frame in pcm.as_chunks_mut::<480>().0 {
            let mut out = [0.0; 480];
            self.apm
                .process_capture_f32(&[frame], &mut [&mut out])
                .map_err(|e| e.to_string())?;
            frame.copy_from_slice(&out);
        }
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn processing_stays_finite_with_render_reference() {
        let s = AudioSettings {
            noise_suppression: true,
            automatic_gain: true,
            echo_cancellation: true,
            ..Default::default()
        };
        let mut p = Processing::new(&s);
        for _ in 0..20 {
            p.render(&[0.0; 480]).unwrap();
            let mut input = [0.001; 960];
            p.capture(&mut input).unwrap();
            assert!(input.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
        }
    }
}
