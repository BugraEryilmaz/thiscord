//! Built-in sounds available in every guild: openly licensed (CC0) recordings
//! bundled in `backend/assets/soundboard` (see its README for provenance). They
//! are normalized like uploads once per process and never stored in the database.
//! They do not count toward a guild's limit and cannot be deleted.
use super::decode::{self, Clip};
use std::sync::OnceLock;
use thiscord_shared::{
    SoundId,
    soundboard::{Sound, encode_pcm},
};

pub(super) struct Builtin {
    pub sound: Sound,
    pub pcm: Vec<u8>,
}

/// Stable wire IDs; never reuse one for a different sound.
const DEFINITIONS: [(u128, &str, &str, &[u8]); 4] = [
    (
        0x7468_6973_636f_7264_8000_0000_0000_0001,
        "cricket",
        "🦗",
        include_bytes!("../../assets/soundboard/cricket.ogg"),
    ),
    (
        0x7468_6973_636f_7264_8000_0000_0000_0002,
        "clap",
        "👏",
        include_bytes!("../../assets/soundboard/clap.ogg"),
    ),
    (
        0x7468_6973_636f_7264_8000_0000_0000_0003,
        "sad horn",
        "🎺",
        include_bytes!("../../assets/soundboard/sad-horn.mp3"),
    ),
    (
        0x7468_6973_636f_7264_8000_0000_0000_0004,
        "ba dum tss",
        "🥁",
        include_bytes!("../../assets/soundboard/ba-dum-tss.ogg"),
    ),
];

/// Decoded on first use, on a blocking worker (every caller runs in one).
pub(super) fn all() -> &'static [Builtin] {
    static SOUNDS: OnceLock<Vec<Builtin>> = OnceLock::new();
    SOUNDS.get_or_init(|| {
        DEFINITIONS
            .iter()
            .map(|(id, name, emoji, file)| {
                let Clip {
                    samples,
                    duration_ms,
                } = decode::prepare(file.to_vec()).expect("bundled sounds decode");
                Builtin {
                    sound: Sound {
                        id: SoundId::from_uuid(uuid::Uuid::from_u128(*id)),
                        name: (*name).into(),
                        emoji: Some((*emoji).into()),
                        duration_ms,
                        created_by: None,
                        created_at: chrono::DateTime::UNIX_EPOCH,
                        builtin: true,
                    },
                    pcm: encode_pcm(&samples),
                }
            })
            .collect()
    })
}

pub(super) fn find(id: SoundId) -> Option<&'static Builtin> {
    all().iter().find(|b| b.sound.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use thiscord_shared::soundboard::{
        MAX_SAMPLES, SAMPLE_RATE, decode_pcm, valid_emoji, valid_name,
    };

    #[test]
    fn bundled_sounds_decode_bounded_leveled_and_distinct() {
        let sounds = all();
        let names: Vec<_> = sounds.iter().map(|b| b.sound.name.as_str()).collect();
        assert_eq!(names, ["cricket", "clap", "sad horn", "ba dum tss"]);
        for (index, builtin) in sounds.iter().enumerate() {
            assert!(builtin.sound.builtin);
            assert!(valid_name(&builtin.sound.name));
            assert!(builtin.sound.emoji.as_deref().is_some_and(valid_emoji));
            let samples = decode_pcm(&builtin.pcm).unwrap();
            assert!(
                samples.len() <= MAX_SAMPLES && samples.len() > SAMPLE_RATE as usize,
                "{} has {} samples",
                builtin.sound.name,
                samples.len()
            );
            // Leveled like uploads: at the loudness target unless the peak ceiling binds.
            let peak = samples.iter().map(|s| s.unsigned_abs()).max().unwrap();
            let energy: f64 = samples.iter().map(|s| f64::from(*s).powi(2)).sum();
            let rms = (energy / samples.len() as f64).sqrt() / 32768.0;
            assert!(
                (0.11..=0.14).contains(&rms) || (28_500..=29_200).contains(&peak),
                "{} rms {rms} peak {peak}",
                builtin.sound.name
            );
            assert!(find(builtin.sound.id).is_some_and(|b| std::ptr::eq(b, builtin)));
            for other in &sounds[index + 1..] {
                assert_ne!(builtin.sound.id, other.sound.id);
                assert_ne!(builtin.pcm, other.pcm);
            }
        }
    }
}
