//! Device format negotiation. Opus/DSP remain fixed at 48 kHz.
use super::{
    device_rate::{MAX_RATE, MIN_RATE},
    mixer::RATE,
};
use cpal::{SupportedStreamConfig, SupportedStreamConfigRange, traits::DeviceTrait};

const MAX_CHANNELS: u16 = 32;

fn compatible(config: &SupportedStreamConfig) -> bool {
    let sample = config.sample_format();
    (MIN_RATE..=MAX_RATE).contains(&config.sample_rate())
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
        .filter_map(|range| {
            let low = range.min_sample_rate().max(MIN_RATE);
            let high = range.max_sample_rate().min(MAX_RATE);
            (low <= high).then(|| range.with_sample_rate(RATE.clamp(low, high)))
        })
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
                config.sample_rate() != RATE,
                Some(*config) != default,
                config.sample_rate().abs_diff(RATE),
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
        "{direction} \"{name}\" has no supported PCM format at {MIN_RATE}-{MAX_RATE} Hz with 1-{MAX_CHANNELS} channels. Default: {default}. Reported: {reported}. Select another device or compatible OS format."
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
    fn accepts_airpods_capture_without_changing_the_device_rate() {
        let airpods = range(1, 24_000, SampleFormat::F32);
        let default = airpods.with_sample_rate(24_000);
        assert_eq!(select(&[airpods], Some(default), true), Some(default));
        assert_eq!(select(&[], Some(default), true), Some(default));
        assert_eq!(select(&[airpods], Some(default), false), Some(default));
        let full_rate = range(2, RATE, SampleFormat::I16);
        assert_eq!(
            select(&[airpods, full_rate], Some(default), true)
                .unwrap()
                .sample_rate(),
            RATE
        );
    }

    #[test]
    fn rejects_out_of_bounds_rates_dsd_and_channel_counts() {
        for invalid in [
            range(2, MIN_RATE - 1, SampleFormat::F32),
            range(2, MAX_RATE + 1, SampleFormat::F32),
            range(2, RATE, SampleFormat::DsdU8),
            range(0, RATE, SampleFormat::F32),
            range(64, RATE, SampleFormat::F32),
        ] {
            let default = invalid.with_sample_rate(invalid.min_sample_rate());
            assert!(select(&[invalid], Some(default), false).is_none());
        }
    }

    #[test]
    fn negotiates_common_and_unusual_pcm_rates_in_both_directions() {
        for rate in [
            8_000, 11_025, 16_000, 22_050, 24_000, 32_000, 44_100, 47_999, 88_200, 96_000, 176_400,
            192_000, 352_800, 384_000,
        ] {
            for sample in [
                SampleFormat::I8,
                SampleFormat::U8,
                SampleFormat::I16,
                SampleFormat::U16,
                SampleFormat::I24,
                SampleFormat::U24,
                SampleFormat::I32,
                SampleFormat::U32,
                SampleFormat::I64,
                SampleFormat::U64,
                SampleFormat::F32,
                SampleFormat::F64,
            ] {
                for input in [true, false] {
                    let fixed = range(2, rate, sample);
                    let selected = select(&[fixed], None, input).unwrap();
                    assert_eq!(selected.sample_rate(), rate);
                    assert_eq!(selected.sample_format(), sample);
                }
            }
        }
    }

    #[test]
    fn respects_ranges_and_prefers_native_then_os_default() {
        let wide = SupportedStreamConfigRange::new(
            2,
            8_000,
            192_000,
            SupportedBufferSize::Unknown,
            SampleFormat::F32,
        );
        let default = range(2, 44_100, SampleFormat::I16).with_sample_rate(44_100);
        assert_eq!(
            select(&[wide], Some(default), false).unwrap().sample_rate(),
            RATE
        );
        let narrow = SupportedStreamConfigRange::new(
            2,
            80_000,
            96_000,
            SupportedBufferSize::Unknown,
            SampleFormat::F32,
        );
        assert_eq!(
            select(&[narrow], None, false).unwrap().sample_rate(),
            80_000
        );
        assert_eq!(select(&[narrow], Some(default), false), Some(default));
        let invalid = SupportedStreamConfigRange::new(
            2,
            1,
            7_999,
            SupportedBufferSize::Unknown,
            SampleFormat::F32,
        );
        assert!(select(&[invalid], None, false).is_none());
    }
}
