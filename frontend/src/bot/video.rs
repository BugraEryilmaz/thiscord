//! Headless viewers use the normal subscription, RTP and keyframe protocols.
use super::*;
use openh264::{decoder::Decoder, formats::YUVSource};
use thiscord_shared::{
    h264::{Assembler, bounded_parameter_sets, recovery_frame},
    screen::Subscription,
    video_feedback::{Receiver as Feedback, ReceiverSample},
    voice_bots::VideoReceiver,
};

pub(super) const MAX_AGE: Duration = Duration::from_millis(250);
const REORDER_AGE: Duration = Duration::from_millis(40);
const REORDER_PACKETS: usize = 256;

struct Stream {
    stats: VideoReceiver,
    feedback: Feedback,
    synchronized: bool,
    decode_us: u64,
    sampled: Duration,
    previous: (u64, u64, u64, u64, u64),
}
struct State {
    active: bool,
    generation: u64,
    streams: Vec<Stream>,
}
struct Packet {
    packet: rtp::Packet,
    arrived: Instant,
    generation: u64,
}

pub(super) struct Receiver {
    state: Arc<StdMutex<State>>,
    jobs: mpsc::Sender<Option<Packet>>,
    started: Instant,
    profile: ConnectionProfile,
}
impl Drop for Receiver {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap();
        state.active = false;
        state.streams.clear();
    }
}
impl Receiver {
    pub fn new() -> Self {
        let state = Arc::new(StdMutex::new(State {
            active: true,
            generation: 0,
            streams: Vec::new(),
        }));
        let (jobs, mut rx) = mpsc::channel::<Option<Packet>>(512);
        let shared = state.clone();
        // Separate from the microphone codec: video load cannot block audio work.
        tokio::task::spawn_blocking(move || {
            let mut generation = 0;
            let mut decoders = HashMap::<u32, VideoDecoder>::new();
            while let Some(job) = rx.blocking_recv() {
                {
                    let state = shared.lock().unwrap();
                    if !state.active {
                        break;
                    }
                    if generation != state.generation {
                        decoders.clear();
                        generation = state.generation;
                    }
                }
                let Some(job) = job else {
                    continue;
                };
                {
                    let state = shared.lock().unwrap();
                    if job.generation != state.generation
                        || !state.streams.iter().any(|s| matches(&s.stats, &job.packet))
                    {
                        continue;
                    }
                }
                let decoder = decoders.entry(job.packet.header.ssrc).or_default();
                let result = decoder.push(job.packet.clone(), job.arrived);
                let mut state = shared.lock().unwrap();
                if !state.active || state.generation != job.generation {
                    continue;
                }
                if let Some(stream) = state
                    .streams
                    .iter_mut()
                    .find(|s| matches(&s.stats, &job.packet))
                {
                    stream.synchronized = decoder.synchronized;
                    stream.stats.missing_packets += result.lost;
                    stream.stats.decode_errors += result.errors;
                    stream.stats.decoded_frames += result.frames;
                    stream.decode_us += result.decode_us;
                    if let Some((width, height)) = result.dimensions {
                        stream.stats.width = width;
                        stream.stats.height = height;
                    }
                }
            }
        });
        Self {
            state,
            jobs,
            started: Instant::now(),
            profile: ConnectionProfile::Good,
        }
    }

    /// Stream owners and epochs fence queued packets across slot reuse/restarts.
    pub fn update(&mut self, members: &[Participant], id: AccountId, settings: &Settings) -> bool {
        let mut state = self.state.lock().unwrap();
        let mut desired: Vec<_> = members
            .iter()
            .filter(|m| m.account_id != id && !settings.deafened)
            .flat_map(|m| {
                [MediaKind::ScreenVideo, MediaKind::Camera]
                    .into_iter()
                    .filter(|&kind| m.video_active(kind) && m.slot < ROOM_CAPACITY)
                    .map(|kind| (kind, m.slot, m.account_id, m.video_epoch(kind)))
            })
            .collect();
        desired.sort_by_key(|(kind, slot, _, _)| kind.relay_ssrc(*slot));
        let existing: Vec<_> = state
            .streams
            .iter()
            .map(|s| (s.stats.kind, s.stats.slot, s.stats.owner, s.stats.epoch))
            .collect();
        if desired == existing && settings.profile == self.profile {
            return false;
        }
        state.generation = state.generation.wrapping_add(1);
        self.profile = settings.profile;
        let maximum = (settings.profile.video_bits() / desired.len().max(1) as u32)
            .max(thiscord_shared::screen::MIN_VIEW_BITRATE);
        state.streams = desired
            .into_iter()
            .map(|(kind, slot, owner, epoch)| Stream {
                stats: VideoReceiver {
                    kind,
                    slot,
                    owner,
                    epoch,
                    bitrate: maximum,
                    received_packets: 0,
                    dropped_packets: 0,
                    missing_packets: 0,
                    decoded_frames: 0,
                    decode_errors: 0,
                    keyframe_requests: 0,
                    width: 0,
                    height: 0,
                },
                feedback: Feedback::with_maximum(maximum),
                synchronized: false,
                decode_us: 0,
                sampled: self.started.elapsed(),
                previous: (0, 0, 0, 0, 0),
            })
            .collect();
        let _ = self.jobs.try_send(None); // Release decoder buffers even when no more media arrives.
        true
    }

    pub fn received(&self, packet: &rtp::Packet, dropped: bool) {
        let mut state = self.state.lock().unwrap();
        if let Some(s) = state.streams.iter_mut().find(|s| matches(&s.stats, packet)) {
            s.stats.received_packets += 1;
            s.stats.dropped_packets += u64::from(dropped);
        }
    }
    pub fn push(&self, packet: rtp::Packet) {
        let mut state = self.state.lock().unwrap();
        let generation = state.generation;
        if let Some(s) = state
            .streams
            .iter_mut()
            .find(|s| matches(&s.stats, &packet))
            && self
                .jobs
                .try_send(Some(Packet {
                    packet,
                    arrived: Instant::now(),
                    generation,
                }))
                .is_err()
        {
            s.stats.dropped_packets += 1;
            s.synchronized = false;
        }
    }

    /// Called every 500 ms, below the ordinary viewer's three-second lease.
    pub fn feedback(&self, bot: &Arc<StdMutex<Bot>>) -> (Vec<Subscription>, Vec<ClientEvent>) {
        let now = self.started.elapsed();
        let mut state = self.state.lock().unwrap();
        let mut requests = Vec::new();
        let views = state
            .streams
            .iter_mut()
            .map(|s| {
                if now.saturating_sub(s.sampled) >= Duration::from_secs(1) {
                    let received = s.stats.received_packets - s.previous.0;
                    let lost = (s.stats.dropped_packets - s.previous.1)
                        .max(s.stats.missing_packets - s.previous.4);
                    let frames = s.stats.decoded_frames - s.previous.2;
                    let work = s.decode_us - s.previous.3;
                    if received > 0 && frames == 0 {
                        s.synchronized = false;
                    }
                    s.feedback.sample_at(
                        ReceiverSample {
                            congested: (lost > 2 && lost * 100 > received) || work > 800_000,
                            severe: lost >= 10 && lost * 100 >= received * 15,
                            receiving: frames > 0,
                        },
                        now,
                    );
                    s.previous = (
                        s.stats.received_packets,
                        s.stats.dropped_packets,
                        s.stats.decoded_frames,
                        s.decode_us,
                        s.stats.missing_packets,
                    );
                    s.sampled = now;
                }
                s.stats.bitrate = s.feedback.bitrate();
                if !s.synchronized {
                    s.stats.keyframe_requests += 1;
                    requests.push(ClientEvent::ScreenKeyframe {
                        kind: s.stats.kind,
                        slot: s.stats.slot,
                        epoch: s.stats.epoch,
                    });
                }
                Subscription {
                    kind: s.stats.kind,
                    slot: s.stats.slot,
                    owner: s.stats.owner,
                    epoch: s.stats.epoch,
                    bitrate: s.stats.bitrate,
                }
            })
            .collect();
        bot.lock().unwrap().video = state.streams.iter().map(|s| s.stats.clone()).collect();
        (views, requests)
    }
}
fn matches(s: &VideoReceiver, packet: &rtp::Packet) -> bool {
    packet.header.ssrc == s.kind.relay_ssrc(s.slot) && packet.header.csrc.first() == Some(&s.epoch)
}

#[derive(Default)]
struct Result {
    lost: u64,
    errors: u64,
    frames: u64,
    decode_us: u64,
    dimensions: Option<(usize, usize)>,
}
#[derive(Default)]
struct VideoDecoder {
    decoder: Option<Decoder>,
    assembler: Assembler,
    reorder: Vec<(rtp::Packet, Instant)>,
    next: Option<u16>,
    timestamp: Option<u32>,
    frame_started: Option<Instant>,
    synchronized: bool,
}
impl VideoDecoder {
    fn reset(&mut self) {
        self.assembler = Assembler::default();
        self.decoder = None;
        self.synchronized = false;
    }
    fn push(&mut self, packet: rtp::Packet, arrived: Instant) -> Result {
        let mut result = Result::default();
        let expected = *self.next.get_or_insert(packet.header.sequence_number);
        if packet.payload.len() > 1500
            || packet.header.sequence_number.wrapping_sub(expected) >= 32768
            || self
                .reorder
                .iter()
                .any(|(p, _)| p.header.sequence_number == packet.header.sequence_number)
        {
            return result;
        }
        self.reorder.push((packet, arrived));
        while !self.reorder.is_empty() {
            let next = self.next.unwrap();
            let index = self
                .reorder
                .iter()
                .enumerate()
                .min_by_key(|(_, (p, _))| p.header.sequence_number.wrapping_sub(next))
                .unwrap()
                .0;
            let gap = self.reorder[index]
                .0
                .header
                .sequence_number
                .wrapping_sub(next);
            if gap != 0 {
                let oldest = self.reorder.iter().map(|(_, at)| *at).min().unwrap();
                if self.reorder.len() < REORDER_PACKETS && oldest.elapsed() < REORDER_AGE {
                    break;
                }
                result.lost += u64::from(gap);
                self.reset();
            }
            let (packet, at) = self.reorder.swap_remove(index);
            self.next = Some(packet.header.sequence_number.wrapping_add(1));
            if at.elapsed() > MAX_AGE {
                result.lost += 1;
                self.reset();
                continue;
            }
            if self.timestamp != Some(packet.header.timestamp) {
                self.timestamp = Some(packet.header.timestamp);
                self.frame_started = Some(at);
            }
            let Some(frame) = self.assembler.push(
                packet.header.timestamp,
                packet.header.sequence_number,
                packet.header.marker,
                &packet.payload,
            ) else {
                if self.assembler.is_damaged() {
                    self.synchronized = false;
                    self.decoder = None;
                }
                continue;
            };
            if self.frame_started.is_none_or(|at| at.elapsed() > MAX_AGE)
                || !bounded_parameter_sets(&frame)
            {
                result.errors += 1;
                self.reset();
                continue;
            }
            if !self.synchronized {
                if !recovery_frame(&frame) {
                    continue;
                }
                self.decoder = Decoder::new().ok();
            }
            let started = Instant::now();
            let decoded = self.decoder.as_mut().map(|d| d.decode(&frame));
            match decoded {
                Some(Ok(Some(image))) => {
                    result.dimensions = Some(image.dimensions());
                    result.frames += 1;
                    self.synchronized = true;
                }
                Some(Ok(None)) => {}
                _ => {
                    result.errors += 1;
                    self.reset();
                }
            }
            result.decode_us += started.elapsed().as_micros() as u64;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rtp::packetizer::Payloader;

    fn frame() -> Vec<u8> {
        use openh264::{
            encoder::{Encoder, EncoderConfig, Profile},
            formats::{RgbSliceU8, YUVBuffer},
        };
        let rgb: Vec<_> = (0..320 * 180 * 3).map(|i| (i % 251) as u8).collect();
        let yuv = YUVBuffer::from_rgb_source(RgbSliceU8::new(&rgb, (320, 180)));
        Encoder::with_api_config(
            openh264::OpenH264API::from_source(),
            EncoderConfig::new().profile(Profile::Baseline),
        )
        .unwrap()
        .encode(&yuv)
        .unwrap()
        .to_vec()
    }
    fn packets(data: Vec<u8>, sequence: &mut u16, timestamp: u32) -> Vec<rtp::Packet> {
        let payloads = rtp::codec::h264::H264Payloader::default()
            .payload(1100, &data.into())
            .unwrap();
        let count = payloads.len();
        payloads
            .into_iter()
            .enumerate()
            .map(|(i, payload)| {
                *sequence = sequence.wrapping_add(1);
                rtp::Packet {
                    header: rtp::header::Header {
                        sequence_number: *sequence,
                        timestamp,
                        marker: i + 1 == count,
                        ..Default::default()
                    },
                    payload,
                }
            })
            .collect()
    }
    #[test]
    fn decoder_reorders_wraps_rejects_loss_and_recovers_without_partial_frames() {
        let data = frame();
        let mut seq = u16::MAX - 2;
        let mut first = packets(data.clone(), &mut seq, 1);
        assert!(first.len() > 4);
        first.swap(1, 2);
        let mut decoder = VideoDecoder::default();
        let mut decoded = 0;
        for p in first {
            decoded += decoder.push(p, Instant::now()).frames;
        }
        assert_eq!(decoded, 1);
        assert!(decoder.synchronized);
        let damaged = packets(data.clone(), &mut seq, 2);
        for (i, p) in damaged.into_iter().enumerate() {
            if i != 1 {
                assert_eq!(decoder.push(p, Instant::now()).frames, 0);
            }
        }
        // Allow a missing sequence to expire; don't decode dependent frames.
        for (_, at) in &mut decoder.reorder {
            *at -= REORDER_AGE;
        }
        let mut recovered = 0;
        let mut lost = 0;
        for p in packets(data.clone(), &mut seq, 3) {
            let result = decoder.push(p, Instant::now());
            recovered += result.frames;
            lost += result.lost;
        }
        assert_eq!(lost, 1);
        assert_eq!(recovered, 1);
        assert!(decoder.synchronized);
        let expired = Instant::now() - MAX_AGE - Duration::from_millis(1);
        for p in packets(data.clone(), &mut seq, 4) {
            assert_eq!(decoder.push(p, expired).frames, 0);
        }
        assert!(!decoder.synchronized);
        for p in packets(data, &mut seq, 5) {
            decoder.push(p, Instant::now());
        }
        assert!(decoder.synchronized);
        // A malformed SPS is rejected before OpenH264 allocation.
        let result = decoder.push(
            rtp::Packet {
                header: rtp::header::Header {
                    sequence_number: seq.wrapping_add(1),
                    timestamp: 6,
                    marker: true,
                    ..Default::default()
                },
                payload: vec![0x67, 100, 0, 31, 255].into(),
            },
            Instant::now(),
        );
        assert_eq!(result.errors, 1);
        assert!(!decoder.synchronized);
    }

    #[test]
    fn malformed_fragments_and_large_sequence_gaps_bound_decoder_work() {
        let mut decoder = VideoDecoder {
            next: Some(0),
            ..Default::default()
        };
        for sequence in 1..2000 {
            decoder.push(
                rtp::Packet {
                    header: rtp::header::Header {
                        sequence_number: sequence * 2,
                        timestamp: 1,
                        marker: false,
                        ..Default::default()
                    },
                    payload: vec![28, 0x85, 1].into(),
                },
                Instant::now(),
            );
            assert!(decoder.reorder.len() < REORDER_PACKETS);
        }
        assert!(!decoder.synchronized);
        assert!(decoder.decoder.is_none());
    }

    #[tokio::test]
    async fn subscriptions_fence_epochs_owners_deafen_and_roster_order() {
        let id: AccountId = "00000000-0000-0000-0000-000000000001".parse().unwrap();
        let owner: AccountId = "00000000-0000-0000-0000-000000000002".parse().unwrap();
        let mut member: Participant = serde_json::from_value(serde_json::json!({
            "account_id": owner, "username":"source", "slot":0, "muted":false,
            "deafened":false,"can_speak":true,"sharing_screen":true,"screen_epoch":1
        }))
        .unwrap();
        let settings = Settings::default();
        let mut receiver = Receiver::new();
        assert!(receiver.update(&[member.clone()], id, &settings));
        let packet = rtp::Packet {
            header: rtp::header::Header {
                ssrc: MediaKind::ScreenVideo.relay_ssrc(0),
                csrc: vec![1],
                ..Default::default()
            },
            ..Default::default()
        };
        receiver.received(&packet, false);
        assert_eq!(
            receiver.state.lock().unwrap().streams[0]
                .stats
                .received_packets,
            1
        );
        assert!(!receiver.update(&[member.clone()], id, &settings));
        member.screen_epoch = 2;
        member.account_id = "00000000-0000-0000-0000-000000000003".parse().unwrap();
        assert!(receiver.update(&[member.clone()], id, &settings));
        receiver.received(&packet, false);
        assert_eq!(
            receiver.state.lock().unwrap().streams[0]
                .stats
                .received_packets,
            0
        );
        let mut other = member.clone();
        other.slot = 1;
        receiver.update(&[member.clone(), other.clone()], id, &settings);
        assert!(!receiver.update(&[other, member.clone()], id, &settings));
        receiver.update(
            &[member],
            id,
            &Settings {
                audio: thiscord_shared::audio::AudioSettings {
                    deafened: true,
                    ..Default::default()
                },
                ..settings
            },
        );
        assert!(receiver.state.lock().unwrap().streams.is_empty());
        let state = receiver.state.clone();
        drop(receiver);
        assert!(!state.lock().unwrap().active);
    }
}
