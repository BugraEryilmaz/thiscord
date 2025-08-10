#[cfg(feature = "diesel")]
use std::{
    ops::Deref,
    sync::{Arc, Mutex as StdMutex},
};

#[cfg(feature = "diesel")]
use diesel::prelude::*;
use partial_modify_derive::PartialModify;
use serde::{Deserialize, Serialize};
#[cfg(feature = "diesel")]
use webrtc_audio_processing::HighPassFilter;

#[derive(Debug, Clone)]
pub enum GlobalAttenuation {
    SelfVoice(i32),  // Attenuation level in dB
    OtherVoice(i32), // Attenuation level in dB
}

#[derive(Debug, Clone)]
pub enum InputMode {
    VoiceActivityDetection(VoiceActivityDetectionCfg), // VAD configuration
    PushToTalk(String),                                // PTT key code
}

#[derive(Debug, Clone)]
pub enum VoiceActivityDetectionCfg {
    Auto,
    Manual {
        threshold: i32, // Threshold in -dB
    },
}

#[derive(Debug, Clone)]
pub struct AudioConfig {
    pub cfg: webrtc_audio_processing::Config,
    pub input_mode: Arc<StdMutex<InputMode>>,
    pub global_attenuation: Arc<StdMutex<Option<GlobalAttenuation>>>,
}

#[cfg(feature = "diesel")]
impl AudioConfig {
    pub fn get(conn: &mut SqliteConnection) -> Self {
        let db_config = AudioConfigDB::get(conn);
        AudioConfig::from(&db_config)
    }

    pub fn save(&self, conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
        let db_config: AudioConfigDB = self.into();
        // Save db_config to the database or configuration file
        db_config.save(conn)?;
        Ok(())
    }
}

#[cfg(feature = "diesel")]
impl AudioConfigDB {
    pub fn get(conn: &mut SqliteConnection) -> Self {
        let db_config = crate::schema::audio_config::table
            .select(AudioConfigDB::as_select())
            .first::<AudioConfigDB>(conn)
            .optional()
            .unwrap_or_default()
            .unwrap_or_default();
        db_config
    }

    pub fn save(&self, conn: &mut SqliteConnection) -> Result<(), diesel::result::Error> {
        // Save db_config to the database or configuration file
        diesel::insert_into(crate::schema::audio_config::table)
            .values(self)
            .on_conflict(crate::schema::audio_config::id)
            .do_update()
            .set(self)
            .execute(conn)?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialModify)]
#[cfg_attr(
    feature = "diesel",
    derive(Queryable, Selectable, Insertable, AsChangeset)
)]
#[partial_modify_derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "diesel", diesel(table_name = crate::schema::audio_config))]
#[cfg_attr(feature = "diesel", diesel(check_for_backend(diesel::sqlite::Sqlite)))]
pub struct AudioConfigDB {
    // WebRTC Audio Processing configuration options
    pub high_pass_filter: bool,
    pub echo_cancellation: bool,
    /// Noise suppression level can be an integer representing the level
    ///
    /// 1: Low, 2: Moderate, 3: High, 4: Very High
    ///
    /// None means no noise suppression
    pub noise_suppression_level: Option<i32>,
    pub gain_controller: bool,
    // Additional fields
    /// Input mode for the audio detection
    ///
    /// 0: Voice Activity Detection (VAD)
    /// 1: Push-to-Talk (PTT)
    pub input_mode: i32,
    /// Push-to-Talk (PTT) key code
    ///
    pub ptt_key_code: Option<String>,
    /// Voice Activity Detection threshold in -dB
    ///
    /// None means automatic detection
    pub vad_threshold: Option<i32>,
    /// Global Attenuation Level in dB
    ///
    /// None means no global attenuation
    pub global_attenuation: Option<i32>,
    /// Global Attenuation Trigger
    ///
    /// None means no global attenuation trigger
    /// 0: Self Voice
    /// 1: Other Voice
    pub global_attenuation_trigger: Option<i32>,
}

impl Default for AudioConfigDB {
    fn default() -> Self {
        AudioConfigDB {
            high_pass_filter: true, // This can be set based on user preferences or defaults
            echo_cancellation: true, // This can also be set based on user preferences
            noise_suppression_level: Some(2), // Example level, can be adjusted
            gain_controller: true,  // Assuming gain controller is enabled by default
            input_mode: 0,          // Default input mode
            ptt_key_code: None,     // Default PTT key code is None
            vad_threshold: None,    // Default VAD threshold is None (automatic detection)
            global_attenuation: None, // Default global attenuation is None
            global_attenuation_trigger: None, // Default global attenuation trigger is None
        }
    }
}

impl From<&AudioConfig> for AudioConfigDB {
    fn from(cfg: &AudioConfig) -> Self {
        AudioConfigDB {
            high_pass_filter: cfg.cfg.high_pass_filter.is_some(),
            echo_cancellation: cfg.cfg.echo_canceller.is_some(),
            noise_suppression_level: cfg.cfg.noise_suppression.as_ref().map(|ns| match ns.level {
                webrtc_audio_processing::NoiseSuppressionLevel::Low => 1,
                webrtc_audio_processing::NoiseSuppressionLevel::Moderate => 2,
                webrtc_audio_processing::NoiseSuppressionLevel::High => 3,
                webrtc_audio_processing::NoiseSuppressionLevel::VeryHigh => 4,
            }),
            gain_controller: cfg.cfg.gain_controller.is_some(),
            input_mode: match cfg.input_mode.lock().unwrap().deref() {
                InputMode::VoiceActivityDetection(_) => 0,
                InputMode::PushToTalk(_) => 1,
            },
            ptt_key_code: match cfg.input_mode.lock().unwrap().deref() {
                InputMode::PushToTalk(key_code) => Some(key_code.clone()),
                _ => None,
            },
            vad_threshold: match cfg.input_mode.lock().unwrap().deref() {
                InputMode::VoiceActivityDetection(VoiceActivityDetectionCfg::Manual {
                    threshold,
                }) => Some(threshold.clone()),
                _ => None,
            },
            global_attenuation: match cfg.global_attenuation.lock().unwrap().deref() {
                Some(GlobalAttenuation::SelfVoice(level)) => Some(level.clone()),
                Some(GlobalAttenuation::OtherVoice(level)) => Some(level.clone()),
                None => None,
            },
            global_attenuation_trigger: match cfg.global_attenuation.lock().unwrap().deref() {
                Some(GlobalAttenuation::SelfVoice(_)) => Some(0),
                Some(GlobalAttenuation::OtherVoice(_)) => Some(1),
                None => None,
            },
        }
    }
}

impl From<&AudioConfigDB> for AudioConfig {
    fn from(db_config: &AudioConfigDB) -> Self {
        AudioConfig {
            cfg: webrtc_audio_processing::Config {
                pipeline: webrtc_audio_processing::Pipeline {
                    maximum_internal_processing_rate:
                        webrtc_audio_processing::PipelineProcessingRate::Max48000Hz,
                    multi_channel_capture: false,
                    multi_channel_render: false,
                    capture_downmix_method: 0, // Use average downmix method
                },
                capture_level_adjustment: None,
                high_pass_filter: if db_config.high_pass_filter {
                    Some(HighPassFilter {
                        apply_in_full_band: true,
                    })
                } else {
                    None
                },
                echo_canceller: if db_config.echo_cancellation {
                    Some(webrtc_audio_processing::EchoCanceller::Mobile)
                    // Some(webrtc_audio_processing::EchoCanceller::Full {
                    //     enforce_high_pass_filtering: true,
                    // })
                } else {
                    None
                },
                noise_suppression: if let Some(level) = db_config.noise_suppression_level {
                    Some(webrtc_audio_processing::NoiseSuppression {
                        level: match level {
                            1 => webrtc_audio_processing::NoiseSuppressionLevel::Low,
                            2 => webrtc_audio_processing::NoiseSuppressionLevel::Moderate,
                            3 => webrtc_audio_processing::NoiseSuppressionLevel::High,
                            4 => webrtc_audio_processing::NoiseSuppressionLevel::VeryHigh,
                            _ => webrtc_audio_processing::NoiseSuppressionLevel::Low,
                        },
                        analyze_linear_aec_output: true,
                    })
                } else {
                    None
                },
                gain_controller: if db_config.gain_controller {
                    Some(webrtc_audio_processing::GainController {
                        mode: webrtc_audio_processing::GainControllerMode::AdaptiveDigital,
                        target_level_dbfs: 3,
                        compression_gain_db: 9,
                        enable_limiter: true,
                    })
                } else {
                    None
                },
                aec3_config: None,
            },
            input_mode: Arc::new(StdMutex::new(match db_config.input_mode {
                0 => InputMode::VoiceActivityDetection(
                    if let Some(threshold) = db_config.vad_threshold {
                        VoiceActivityDetectionCfg::Manual { threshold }
                    } else {
                        VoiceActivityDetectionCfg::Auto
                    },
                ),
                1 => {
                    InputMode::PushToTalk(db_config.ptt_key_code.clone().unwrap_or("v".to_string()))
                }
                _ => InputMode::VoiceActivityDetection(
                    if let Some(threshold) = db_config.vad_threshold {
                        VoiceActivityDetectionCfg::Manual { threshold }
                    } else {
                        VoiceActivityDetectionCfg::Auto
                    },
                ), // Default to VAD if invalid
            })),
            global_attenuation: Arc::new(StdMutex::new(
                match db_config.global_attenuation_trigger {
                    Some(trigger) => match trigger {
                        0 => Some(GlobalAttenuation::SelfVoice(
                            db_config.global_attenuation.unwrap_or(0),
                        )),
                        1 => Some(GlobalAttenuation::OtherVoice(
                            db_config.global_attenuation.unwrap_or(0),
                        )),
                        _ => None, // Invalid trigger
                    },
                    None => None, // No global attenuation
                },
            )),
        }
    }
}
