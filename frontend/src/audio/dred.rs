//! Bounded Opus 1.6.1 deep redundancy policy, exclusively on native workers.
use super::{codec::DecodeStats, jitter::Packet, mixer::FRAME};
use bytes::Bytes;

pub const HISTORY: u32 = 3 * FRAME as u32;
pub const TARGET_MS: f64 = 80.0;
pub const BITRATE: i32 = 40_000;
pub const DURATION: i32 = 6; // 60 ms requested, in 10 ms units.

impl super::Session {
    pub(super) fn set_deep_redundancy(&mut self, enabled: bool) -> Result<(), String> {
        if self.settings.deep_redundancy != enabled {
            self.reset_gate(std::time::Instant::now());
            self.encoder
                .set_deep_redundancy(enabled)
                .map_err(|e| e.to_string())?;
            for (index, (remote, writer)) in self
                .remotes
                .iter_mut()
                .zip(&self.devices.writers)
                .enumerate()
            {
                if let Some(remote) = remote {
                    remote.set_deep_redundancy(
                        writer,
                        enabled && index < thiscord_shared::voice::ROOM_CAPACITY,
                    )?;
                }
            }
        }
        Ok(())
    }
}

/// Use the closest queued successor only. Sequence distance must account for
/// every missing media slot, not an intentional DTX/capture timestamp jump.
/// No search beyond a talkspurt/silence boundary and no retained packet history.
pub fn donor(
    packets: &[Packet],
    cursor: u32,
    last: Option<u16>,
    missing: u32,
) -> Option<(Bytes, u32)> {
    let packet = packets
        .iter()
        .min_by_key(|p| p.timestamp.wrapping_sub(cursor))?;
    let offset = packet.timestamp.wrapping_sub(cursor);
    let sequence_gap = packet.sequence.wrapping_sub(last?) as i16;
    if packet.marker
        || packet.no_data
        || !(FRAME as u32..=HISTORY).contains(&offset)
        || !offset.is_multiple_of(FRAME as u32)
        || sequence_gap <= 0
        || u32::from(sequence_gap as u16) != (missing + offset) / FRAME as u32 + 1
    {
        return None;
    }
    Some((packet.payload.clone(), offset))
}

/// True means a covered DRED interval was synthesized, not a bit-exact recovery.
/// The temporary parser/feature object is dropped on every call. All persistent
/// neural synthesis history belongs to OpusDecoder and its existing full reset.
pub fn decode(
    decoder: &mut opus::Decoder,
    packet: &[u8],
    offset: u32,
    pcm: &mut [f32; FRAME],
    stats: &mut DecodeStats,
) -> bool {
    stats.dred_attempts += 1;
    if !opus::packet::get_nb_samples(packet, 48000).is_ok_and(|n| n == FRAME) {
        stats.decode_errors += 1;
        return false;
    }
    let result = (|| {
        let Some(data) = opus::Dred::parse(packet, HISTORY as i32, 48000)? else {
            return Ok(None);
        };
        data.decode_float(decoder, offset as i32, pcm)
    })();
    match result {
        Ok(Some(FRAME)) if pcm.iter().all(|x| x.is_finite()) => {
            stats.dred_output_frames += 1;
            true
        }
        Ok(None) => false,
        _ => {
            stats.decode_errors += 1;
            let _ = decoder.reset_state();
            pcm.fill(0.0);
            false
        }
    }
}

#[cfg(test)]
mod tests;
