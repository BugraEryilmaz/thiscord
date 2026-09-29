//! Device format negotiation. Opus/DSP remain fixed at 48 kHz.
use super::mixer::RATE;
use cpal::{SupportedStreamConfig, SupportedStreamConfigRange, traits::DeviceTrait};

const MAX_CHANNELS: u16 = 32;

fn compatible(config: &SupportedStreamConfig) -> bool {
    let sample = config.sample_format();
    config.sample_rate() == RATE
        && (1..=MAX_CHANNELS).contains(&config.channels())
        && (sample.is_int() || sample.is_uint() || sample.is_float())
}

fn select(
    ranges: &[SupportedStreamConfigRange],
    default: Option<SupportedStreamConfig>,
    input: bool,
) -> Option<SupportedStreamConfig> {
    ranges
        .iter()
        .filter_map(|range| range.try_with_sample_rate(RATE))
        .chain(default)
        .filter(compatible)
        .min_by_key(|config| {
            let channels = config.channels();
            // Prefer mono capture/stereo playback; fall back to multichannel
            // devices instead of incorrectly blaming their sample rate.
            let channel_rank = if channels == if input { 1 } else { 2 } {
                0
            } else if channels <= 2 {
                1
            } else {
                channels
            };
            (
                channel_rank,
                config.sample_format() != cpal::SampleFormat::F32,
            )
        })
}

pub(super) fn config(device: &cpal::Device, input: bool) -> Result<SupportedStreamConfig, String> {
    let direction = if input { "Microphone" } else { "Output" };
    let name: String = device
        .description()
        .map(|description| description.name().chars().take(80).collect())
        .unwrap_or_else(|_| "selected device".into());
    let ranges: Result<Vec<_>, _> = if input {
        device.supported_input_configs().map(Iterator::collect)
    } else {
        device.supported_output_configs().map(Iterator::collect)
    };
    let default = if input {
        device.default_input_config()
    } else {
        device.default_output_config()
    }
    .ok();
    // Some backends expose a usable default even if range enumeration is empty
    // or unavailable. Never invent a format or change its reported sample rate.
    if let Some(config) = select(ranges.as_deref().unwrap_or(&[]), default, input) {
        return Ok(config);
    }
    let default = default
        .map(|c| {
            format!(
                "{} Hz, {} channels, {}",
                c.sample_rate(),
                c.channels(),
                c.sample_format()
            )
        })
        .unwrap_or_else(|| "unavailable".into());
    let reported = match ranges {
        Ok(ranges) if !ranges.is_empty() => ranges
            .iter()
            .take(6)
            .map(|c| {
                format!(
                    "{}-{} Hz/{} channels/{}",
                    c.min_sample_rate(),
                    c.max_sample_rate(),
                    c.channels(),
                    c.sample_format()
                )
            })
            .collect::<Vec<_>>()
            .join(", "),
        Ok(_) => "no formats reported".into(),
        Err(error) => format!("format query failed ({:?})", error.kind()),
    };
    Err(format!(
        "{direction} \"{name}\" has no supported 48 kHz PCM format with 1-{MAX_CHANNELS} channels. Default: {default}. Reported: {reported}. Select another device or compatible OS format."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use cpal::{SampleFormat, SupportedBufferSize};

    fn range(channels: u16, rate: u32, format: SampleFormat) -> SupportedStreamConfigRange {
        SupportedStreamConfigRange::new(channels, rate, rate, SupportedBufferSize::Unknown, format)
    }

    #[test]
    fn accepts_48khz_integer_and_surround_devices() {
        for sample in [SampleFormat::I24, SampleFormat::I32, SampleFormat::F64] {
            for channels in [1, 2, 6, 8] {
                for input in [true, false] {
                    let selected = select(&[range(channels, RATE, sample)], None, input).unwrap();
                    assert_eq!(selected.channels(), channels);
                    assert_eq!(selected.sample_format(), sample);
                    assert_eq!(selected.sample_rate(), RATE);
                }
            }
        }
    }

    #[test]
    fn prefers_mono_capture_and_stereo_playback_over_surround() {
        let ranges = [
            range(8, RATE, SampleFormat::F32),
            range(2, RATE, SampleFormat::I24),
            range(1, RATE, SampleFormat::F32),
        ];
        assert_eq!(select(&ranges, None, true).unwrap().channels(), 1);
        assert_eq!(select(&ranges, None, false).unwrap().channels(), 2);
    }

    #[test]
    fn uses_a_compatible_default_when_enumeration_is_empty() {
        let default = range(2, RATE, SampleFormat::I32).with_sample_rate(RATE);
        assert_eq!(select(&[], Some(default), false), Some(default));
    }

    #[test]
    fn rejects_wrong_rates_dsd_and_unbounded_channel_counts() {
        for invalid in [
            range(2, 44_100, SampleFormat::F32),
            range(2, RATE, SampleFormat::DsdU8),
            range(0, RATE, SampleFormat::F32),
            range(64, RATE, SampleFormat::F32),
        ] {
            let default = invalid.with_sample_rate(invalid.min_sample_rate());
            assert!(select(&[invalid], Some(default), false).is_none());
        }
    }
}
