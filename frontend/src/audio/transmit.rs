//! Native microphone DTX policy; timing and admission live in outgoing.
use super::mixer::FRAME;
use std::time::Instant;

#[derive(Default, serde::Serialize)]
pub struct Stats {
    pub encoded_frames: u64,
    pub encoded_bytes: u64,
    pub omitted_packets: u64,
    pub omitted_bytes: u64,
    pub encode_ns: u64,
    pub queued_packets: u64,
    pub queued_bytes: u64,
    pub queue_drops: u64,
}

pub struct Encoder {
    pub(crate) adaptive: super::adaptive_fec::Controller,
    pub codec: opus::Encoder,
    pub stats: Stats,
    in_dtx: bool,
    no_data_sent: bool,
    dtx_enabled: bool,
}
impl Encoder {
    pub fn new(dtx: bool) -> Result<Self, opus::Error> {
        Ok(Self {
            adaptive: Default::default(),
            codec: {
                let mut codec = super::codec::microphone_encoder()?;
                codec.set_dtx(dtx)?;
                codec
            },
            stats: Stats::default(),
            in_dtx: false,
            no_data_sent: false,
            dtx_enabled: dtx,
        })
    }
    pub fn reset(&mut self) -> Result<(), opus::Error> {
        self.in_dtx = false;
        self.no_data_sent = false;
        self.codec.reset_state()
    }
    /// The sole live writer of the Opus loss-hint CTL. Integration passes floor
    /// 20 when DRED is enabled, otherwise 5; codec bitrate belongs to that policy.
    pub(crate) fn adapt(
        &mut self,
        enabled: bool,
        floor: u8,
        sample: Option<super::adaptive_fec::Sample>,
        now: Instant,
    ) -> Result<(), opus::Error> {
        let hint = self.adaptive.update(enabled, floor, sample, now);
        if self.codec.get_packet_loss_perc()? != i32::from(hint) {
            self.codec.set_packet_loss_perc(i32::from(hint))?;
        }
        Ok(())
    }
    pub fn encode(
        &mut self,
        pcm: &[f32; FRAME],
        protect_speech: bool,
        output: &mut [u8],
    ) -> Result<(usize, bool, bool), opus::Error> {
        let start = Instant::now();
        // Do not let codec VAD discard quiet speech/pre-roll admitted by our gate.
        self.codec.set_dtx(self.dtx_enabled && !protect_speech)?;
        let n = self.codec.encode_float(pcm, output)?;
        let dtx = self.codec.get_in_dtx()?;
        self.stats.encode_ns += start.elapsed().as_nanos() as u64;
        self.stats.encoded_frames += 1;
        self.stats.encoded_bytes += n as u64;
        // libopus 1.6.1 opus.h explicitly permits omitting <=2-byte results.
        // GET_IN_DTX alone is NOT sufficient: it includes mandatory CN updates.
        // Forward one no-data packet after each CN update. It gives the receiver
        // a valid Opus silence cue without a new RTP extension or wire contract.
        // Repeating it once per update bounds recovery if that cue is lost.
        let no_data = dtx && n <= 2;
        let omit = no_data && self.no_data_sent;
        self.no_data_sent = no_data;
        let marker = self.in_dtx && !dtx;
        self.in_dtx = dtx;
        if omit {
            self.stats.omitted_packets += 1;
            self.stats.omitted_bytes += n as u64;
        }
        Ok((n, omit, marker))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn real_opus_silence_keeps_updates_and_protects_quiet_and_abrupt_onsets() {
        let mut encoder = Encoder::new(true).unwrap();
        assert!(encoder.codec.get_inband_fec().unwrap());
        assert_eq!(encoder.codec.get_packet_loss_perc().unwrap(), 5);
        let mut output = [0; 4000];
        let mut last_sent = 0;
        for frame in 0..3000 {
            let (n, omitted, _) = encoder.encode(&[0.0; FRAME], false, &mut output).unwrap();
            if !omitted {
                last_sent = frame;
                assert!(n > 0);
            }
            assert!(
                frame - last_sent <= 21,
                "comfort-noise updates must survive"
            );
        }
        assert!(encoder.stats.omitted_packets > 2600);
        // Silence DTX may not second-guess either quiet pre-roll or loud onset.
        for level in [0.0001, 0.2] {
            let pcm = std::array::from_fn(|i| (i as f32 * 0.04).sin() * level);
            let (_, omitted, _) = encoder.encode(&pcm, true, &mut output).unwrap();
            assert!(!omitted);
        }
        let mut continuous = Encoder::new(false).unwrap();
        for _ in 0..100 {
            assert!(
                !continuous
                    .encode(&[0.0; FRAME], false, &mut output)
                    .unwrap()
                    .1
            );
        }
    }
}
