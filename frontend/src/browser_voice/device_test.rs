//! Audio settings device tests outside calls, like the desktop: a 5-second
//! playback tone, or a 60-second microphone test with meters and optional
//! loopback through the selected output. Start from a click (`prime`) so the
//! AudioContext may run.
use super::media::{Microphone, Output};
use std::cell::Cell;
use thiscord_frontend::{browser_audio as rules, browser_call::Capture};
use thiscord_shared::audio::AudioSettings;
use wasm_bindgen::JsCast;
use web_sys::AudioBufferSourceNode;

pub(super) struct DeviceTest {
    pub id: u64,
    pub output: Output,
    pub microphone: Option<Microphone>,
    tone: Option<AudioBufferSourceNode>,
    started: f64,
    length_ms: f64,
    stopped: Cell<bool>,
}
impl DeviceTest {
    pub async fn start(
        id: u64,
        output: Output,
        settings: &AudioSettings,
        microphone: bool,
        loopback: bool,
    ) -> Result<Self, String> {
        output.set_sink(settings.output.as_deref().unwrap_or_default());
        output.set_master(settings.output_volume, settings.deafened);
        let mut test = Self {
            id,
            output,
            microphone: None,
            tone: None,
            started: js_sys::Date::now(),
            length_ms: if microphone {
                rules::MICROPHONE_TEST_MS
            } else {
                rules::PLAYBACK_TEST_MS
            },
            stopped: Cell::new(false),
        };
        if microphone {
            let opened =
                Microphone::open(&Capture::from_settings(settings), test.output.context()).await;
            match opened {
                Ok(microphone) => {
                    microphone.set_boost(settings.input_gain);
                    microphone.set_sending(false, true);
                    if loopback {
                        microphone.loopback(test.output.input());
                    }
                    test.microphone = Some(microphone);
                }
                Err(_) => {
                    test.stop();
                    return Err("Cannot open the microphone. Allow microphone access for this site and check the selected device.".into());
                }
            }
        } else {
            let rate = test.output.sample_rate().unwrap_or(48_000.0);
            test.tone = test
                .output
                .buffer(&rules::test_tone(rate as u32), rate)
                .and_then(|buffer| test.output.play_test(&buffer));
            if test.tone.is_none() {
                test.stop();
                return Err("Cannot play audio in this browser".into());
            }
        }
        Ok(test)
    }
    pub fn expired(&self) -> bool {
        js_sys::Date::now() - self.started > self.length_ms
    }
    pub fn length_ms(&self) -> f64 {
        self.length_ms
    }
    pub fn apply(&self, settings: &AudioSettings) {
        self.output
            .set_master(settings.output_volume, settings.deafened);
        if let Some(microphone) = &self.microphone {
            microphone.set_boost(settings.input_gain);
        }
    }
    pub fn stop(&self) {
        if self.stopped.replace(true) {
            return;
        }
        if let Some(tone) = &self.tone {
            let _ = tone
                .unchecked_ref::<web_sys::AudioScheduledSourceNode>()
                .stop();
        }
        if let Some(microphone) = &self.microphone {
            microphone.loopback(None);
            microphone.stop();
        }
        self.output.close();
    }
}
impl Drop for DeviceTest {
    fn drop(&mut self) {
        self.stop();
    }
}
