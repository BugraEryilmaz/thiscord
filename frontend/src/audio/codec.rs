//! Native worker-only Opus policy. See docs/audio.md for the fixed FEC budget.
use super::{
    jitter::Playout,
    mixer::{FRAME, RATE},
};

#[cfg(test)]
#[path = "fec_tests.rs"]
mod tests;

pub(super) fn microphone_encoder() -> Result<opus::Encoder, opus::Error> {
    let mut encoder = opus::Encoder::new(RATE, opus::Channels::Mono, opus::Application::Voip)?;
    encoder.set_bitrate(opus::Bitrate::Bits(32_000))?;
    encoder.set_inband_fec(true)?;
    encoder.set_packet_loss_perc(5)?;
    Ok(encoder)
}

/// Successful FEC calls can contain PLC: libopus does not report which was used.
/// These counters intentionally never claim a verified recovery count.
#[derive(Default, serde::Serialize)]
pub(super) struct DecodeStats {
    pub normal_frames: u64,
    pub fec_attempts: u64,
    pub fec_output_frames: u64,
    pub plc_frames: u64,
    pub decode_errors: u64,
    pub silence_frames: u64,
}

/// Returns true only for a successfully decoded normal packet (activity input).
/// Full intervals produce FRAME samples, including failures; fractional gaps
/// produce silence without advancing the Opus state by an invented full frame.
pub(super) fn decode(
    decoder: &mut opus::Decoder,
    decision: &Playout,
    pcm: &mut [f32; FRAME],
    stats: &mut DecodeStats,
) -> bool {
    let (packet, fec) = match decision {
        Playout::Packet(packet) => (packet.as_ref(), false),
        Playout::Fec(packet) => {
            stats.fec_attempts += 1;
            (packet.as_ref(), true)
        }
        Playout::Conceal(FRAME) => (&[][..], false),
        Playout::Conceal(_) => {
            pcm.fill(0.0);
            return false;
        }
    };
    if !matches!(decision, Playout::Conceal(_)) {
        // Reject malformed/variable-duration packets before changing decoder state.
        // Our transport emits one 20 ms frame; FEC/PLC must use that exact duration.
        if !packet.is_empty() && decoder.get_nb_samples(packet).is_ok_and(|n| n == FRAME) {
            if decoder
                .decode_float(packet, pcm, fec)
                .is_ok_and(|n| n == FRAME)
                && pcm.iter().all(|s| s.is_finite())
            {
                if fec {
                    stats.fec_output_frames += 1;
                } else {
                    stats.normal_frames += 1;
                }
                return !fec;
            }
            // A failed decode may have touched state. Do not reuse partial output.
            let _ = decoder.reset_state();
        }
        stats.decode_errors += 1;
    }
    if decoder
        .decode_float(&[], pcm, false)
        .is_ok_and(|n| n == FRAME)
        && pcm.iter().all(|s| s.is_finite())
    {
        stats.plc_frames += 1;
    } else {
        stats.decode_errors += 1;
        stats.silence_frames += 1;
        let _ = decoder.reset_state();
        pcm.fill(0.0);
    }
    false
}
