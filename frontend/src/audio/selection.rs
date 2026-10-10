//! Resolve saved endpoints on the opening worker, without changing preferences.
use super::*;

pub(super) struct Selected<T> {
    pub device: T,
    pub fallback: bool,
}

pub(super) fn device(id: Option<&str>, input: bool) -> Result<Selected<cpal::Device>, String> {
    let host = cpal::default_host();
    select(
        id,
        input,
        |id| {
            Ok(host
                .devices()
                .map_err(|e| e.to_string())?
                .find(|d| d.id().is_ok_and(|v| v.to_string() == id)))
        },
        || {
            if input {
                host.default_input_device()
            } else {
                host.default_output_device()
            }
        },
    )
}

fn select<T>(
    id: Option<&str>,
    input: bool,
    find: impl FnOnce(&str) -> Result<Option<T>, String>,
    default: impl FnOnce() -> Option<T>,
) -> Result<Selected<T>, String> {
    if let Some(id) = id
        && let Some(device) = find(id)?
    {
        return Ok(Selected {
            device,
            fallback: false,
        });
    }
    default()
        .map(|device| Selected {
            device,
            fallback: id.is_some(),
        })
        .ok_or_else(|| {
            let direction = if input { "microphone" } else { "output" };
            if id.is_some() {
                format!("Selected {direction} is disconnected or unavailable, and no system default is available. Connect an audio device and join again.")
            } else {
                format!("No system default {direction} is available. Connect an audio device and join again.")
            }
        })
}

pub(super) fn fallback_notice(info: &serde_json::Value) -> Option<String> {
    let input = info["input"]["fallback"].as_bool().unwrap_or(false);
    let output = info["output"]["fallback"].as_bool().unwrap_or(false);
    let missing = match (input, output) {
        (true, true) => "microphone and output are",
        (true, false) => "microphone is",
        (false, true) => "output is",
        (false, false) => return None,
    };
    Some(format!(
        "Selected {missing} unavailable; using system defaults where needed. Microphone: {}; output: {}. Saved device preferences unchanged.",
        info["input"]["name"].as_str().unwrap_or("not active"),
        info["output"]["name"].as_str().unwrap_or("default")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unplugged_headset_falls_back_only_in_the_missing_direction() {
        for missing_input in [true, false] {
            for input in [true, false] {
                let selected = select(
                    Some("saved endpoint"),
                    input,
                    |_| Ok((input != missing_input).then_some("saved endpoint")),
                    || Some("system default"),
                )
                .unwrap();
                assert_eq!(selected.fallback, input == missing_input);
                assert_eq!(
                    selected.device,
                    if input == missing_input {
                        "system default"
                    } else {
                        "saved endpoint"
                    }
                );
            }
        }
    }

    #[test]
    fn rejoining_after_replug_uses_saved_device_again() {
        let saved = AudioSettings {
            output: Some("headset".into()),
            ..Default::default()
        };
        for connected in [false, false, true] {
            let selected = select(
                saved.output.as_deref(),
                false,
                |_| Ok(connected.then_some("headset")),
                || Some("speakers"),
            )
            .unwrap();
            assert_eq!(
                selected.device,
                if connected { "headset" } else { "speakers" }
            );
            assert_eq!(selected.fallback, !connected);
        }
        assert_eq!(saved.output.as_deref(), Some("headset"));
    }

    #[test]
    fn default_selection_never_enumerates_saved_devices_or_reports_fallback() {
        let selected = select(
            None,
            false,
            |_| panic!("no saved selection"),
            || Some("speakers"),
        )
        .unwrap();
        assert_eq!(selected.device, "speakers");
        assert!(!selected.fallback);
    }

    #[test]
    fn missing_defaults_and_enumeration_errors_remain_recoverable_errors() {
        for input in [true, false] {
            let error = select::<()>(Some("unplugged"), input, |_| Ok(None), || None)
                .err()
                .unwrap();
            assert!(error.contains("no system default"));
            assert!(error.contains(if input { "microphone" } else { "output" }));
        }
        let error = select::<()>(
            Some("saved"),
            false,
            |_| Err("enumeration failed".into()),
            || panic!("cannot establish that the device is missing"),
        )
        .err()
        .unwrap();
        assert_eq!(error, "enumeration failed");
    }
}
