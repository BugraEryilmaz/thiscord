//! Voice-message container rules shared by recorders, the backend validator and
//! players: Ogg Opus muxing/parsing, WebM→Ogg remuxing (browser recorders),
//! Ogg→WebM remuxing (browsers that cannot play Ogg), Opus TOC durations and the
//! compact waveform. Pure Rust without platform APIs; nothing here decodes audio.
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

/// Longest accepted recording. Recorders stop on their own at this length.
pub const MAX_VOICE_MESSAGE_MS: u32 = 300_000;
/// Shorter taps are discarded by recorders and rejected by the backend.
pub const MIN_VOICE_MESSAGE_MS: u32 = 500;
/// Encoders may add up to one packet of padding and the lookahead.
const DURATION_TOLERANCE_MS: u32 = 1_000;
/// Five minutes of 32 kbps mono Opus need about 1.2 MB; browsers may choose
/// higher bitrates. Enforced for uploads flagged as voice messages.
pub const MAX_VOICE_MESSAGE_BYTES: usize = 5 * 1024 * 1024;
/// Browser recorder output accepted for remuxing before the Ogg limit applies.
pub const MAX_RECORDER_BYTES: usize = 2 * MAX_VOICE_MESSAGE_BYTES;
/// Number of amplitude bars in [`crate::chat::VoiceMetadata::waveform`].
pub const WAVEFORM_BARS: usize = 64;
pub const VOICE_MESSAGE_FILENAME: &str = "voice-message.ogg";
pub const VOICE_MESSAGE_MIME: &str = "audio/ogg";
pub const SAMPLE_RATE: u32 = 48_000;
/// 2.5 ms CELT frames bound the packet count of a maximal recording.
const MAX_PACKETS: usize =
    (MAX_VOICE_MESSAGE_MS as usize + DURATION_TOLERANCE_MS as usize) * 2 / 5 + 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Samples per channel at 48 kHz in one Opus packet (RFC 6716 §3.1), or None
/// for malformed packets and packets longer than 120 ms.
pub fn opus_packet_samples(packet: &[u8]) -> Option<u32> {
    let toc = *packet.first()?;
    let config = toc >> 3;
    let frame = match config {
        0..=11 => [480, 960, 1920, 2880][usize::from(config & 3)],
        12..=15 => [480, 960][usize::from(config & 1)],
        _ => [120, 240, 480, 960][usize::from(config & 3)],
    };
    let frames = match toc & 3 {
        0 => 1,
        1 | 2 => 2,
        _ => u32::from(*packet.get(1)? & 0x3f),
    };
    let samples = frame * frames;
    (frames > 0 && samples <= 5_760).then_some(samples)
}

/// Opus identification header (RFC 7845 §5.1) for mono/stereo mapping family 0.
pub fn opus_head(channels: u8, pre_skip: u16) -> Vec<u8> {
    let mut head = b"OpusHead".to_vec();
    head.push(1);
    head.push(channels);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes());
    head.push(0);
    head
}

fn parse_head(head: &[u8]) -> Result<(u8, u16), Error> {
    let invalid = Error("Voice message has an invalid Opus header");
    if head.len() < 19 || !head.starts_with(b"OpusHead") || head[8] >> 4 != 0 {
        return Err(invalid);
    }
    let channels = head[9];
    // Family 0 only: one Opus stream, mono or stereo, no channel mapping table.
    if !(1..=2).contains(&channels) || head[18] != 0 {
        return Err(Error("Voice messages must be mono or stereo Opus"));
    }
    Ok((channels, u16::from_le_bytes([head[10], head[11]])))
}

const fn crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = if value & 0x8000_0000 != 0 {
                (value << 1) ^ 0x04c1_1db7
            } else {
                value << 1
            };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
}
static CRC: [u32; 256] = crc_table();
fn ogg_crc(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0, |crc, byte| {
        (crc << 8) ^ CRC[usize::from(((crc >> 24) as u8) ^ byte)]
    })
}

/// Streams Opus packets into an in-memory Ogg file. Audio pages hold about one
/// second of packets; headers get their own pages as RFC 7845 requires.
pub struct OggOpusWriter {
    out: Vec<u8>,
    serial: u32,
    sequence: u32,
    granule: u64,
    packets: Vec<u8>,
    lacing: Vec<u8>,
    queued: usize,
}
impl OggOpusWriter {
    pub fn new(serial: u32, channels: u8, pre_skip: u16) -> Self {
        let mut writer = Self {
            out: Vec::new(),
            serial,
            sequence: 0,
            granule: 0,
            packets: Vec::new(),
            lacing: Vec::new(),
            queued: 0,
        };
        writer.page(&opus_head(channels, pre_skip), 0x02, 0);
        let mut tags = b"OpusTags".to_vec();
        let vendor = b"thiscord";
        tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        tags.extend_from_slice(vendor);
        tags.extend_from_slice(&0u32.to_le_bytes());
        writer.page(&tags, 0, 0);
        writer
    }

    fn page(&mut self, packet: &[u8], flags: u8, granule: u64) {
        let mut lacing = vec![255u8; packet.len() / 255];
        lacing.push((packet.len() % 255) as u8);
        self.emit(flags, granule, &lacing, packet);
    }

    fn emit(&mut self, flags: u8, granule: u64, lacing: &[u8], data: &[u8]) {
        let start = self.out.len();
        self.out.extend_from_slice(b"OggS");
        self.out.push(0);
        self.out.push(flags);
        self.out.extend_from_slice(&granule.to_le_bytes());
        self.out.extend_from_slice(&self.serial.to_le_bytes());
        self.out.extend_from_slice(&self.sequence.to_le_bytes());
        self.out.extend_from_slice(&[0; 4]);
        self.out.push(lacing.len() as u8);
        self.out.extend_from_slice(lacing);
        self.out.extend_from_slice(data);
        let crc = ogg_crc(&self.out[start..]);
        self.out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        self.sequence += 1;
    }

    fn flush(&mut self, flags: u8) {
        if self.queued == 0 && flags & 0x04 == 0 {
            return;
        }
        let lacing = std::mem::take(&mut self.lacing);
        let packets = std::mem::take(&mut self.packets);
        self.emit(flags, self.granule, &lacing, &packets);
        self.queued = 0;
    }

    /// Appends one packet (at most 1275 bytes per frame, 120 ms) lasting `samples`.
    pub fn push(&mut self, packet: &[u8], samples: u32) {
        let segments = packet.len() / 255 + 1;
        if self.lacing.len() + segments > 255 || self.queued >= 50 {
            self.flush(0);
        }
        self.lacing
            .extend(std::iter::repeat_n(255u8, packet.len() / 255));
        self.lacing.push((packet.len() % 255) as u8);
        self.packets.extend_from_slice(packet);
        self.granule += u64::from(samples);
        self.queued += 1;
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.flush(0x04);
        self.out
    }
}

/// A parsed single-stream Ogg Opus file.
pub struct OggOpus {
    pub channels: u8,
    pub pre_skip: u16,
    pub head: Vec<u8>,
    pub packets: Vec<Vec<u8>>,
    /// Samples per packet at 48 kHz, from each packet's TOC.
    pub samples: Vec<u32>,
    /// Playable length after pre-skip and end trimming, in milliseconds.
    pub duration_ms: u32,
}
impl OggOpus {
    /// Start sample (48 kHz, before pre-skip removal) of each packet.
    pub fn offsets(&self) -> Vec<u64> {
        let mut at = 0u64;
        self.samples
            .iter()
            .map(|s| {
                let start = at;
                at += u64::from(*s);
                start
            })
            .collect()
    }
}

/// Validates the Ogg framing (CRC, one logical stream, header packets, page
/// order) and every packet's TOC. Rejects trailing data and chained streams.
pub fn parse_ogg_opus(bytes: &[u8]) -> Result<OggOpus, Error> {
    let invalid = Error("Voice message is not a valid Ogg Opus file");
    let mut at = 0usize;
    let mut serial = None;
    let mut sequence = 0u32;
    let mut current = Vec::new();
    let mut packets: Vec<Vec<u8>> = Vec::new();
    let mut last_granule = None::<u64>;
    let mut ended = false;
    while at < bytes.len() {
        if ended {
            return Err(invalid);
        }
        let header = bytes.get(at..at + 27).ok_or(invalid)?;
        if &header[..4] != b"OggS" || header[4] != 0 {
            return Err(invalid);
        }
        let flags = header[5];
        let granule = u64::from_le_bytes(header[6..14].try_into().unwrap());
        let page_serial = u32::from_le_bytes(header[14..18].try_into().unwrap());
        let page_sequence = u32::from_le_bytes(header[18..22].try_into().unwrap());
        let crc = u32::from_le_bytes(header[22..26].try_into().unwrap());
        let count = usize::from(header[26]);
        let lacing = bytes.get(at + 27..at + 27 + count).ok_or(invalid)?;
        let size: usize = lacing.iter().map(|v| usize::from(*v)).sum();
        let end = at + 27 + count + size;
        let page = bytes.get(at..end).ok_or(invalid)?;
        let mut check = page.to_vec();
        check[22..26].fill(0);
        if ogg_crc(&check) != crc {
            return Err(invalid);
        }
        match serial {
            None if flags & 0x02 != 0 => serial = Some(page_serial),
            Some(s) if s == page_serial && flags & 0x02 == 0 && page_sequence == sequence => {}
            _ => return Err(invalid),
        }
        sequence = page_sequence.wrapping_add(1);
        // A continued page must follow an unfinished packet, and vice versa.
        if (flags & 0x01 != 0) != !current.is_empty() {
            return Err(invalid);
        }
        let mut data = at + 27 + count;
        for value in lacing {
            current.extend_from_slice(&bytes[data..data + usize::from(*value)]);
            data += usize::from(*value);
            if *value < 255 {
                if packets.len() >= MAX_PACKETS {
                    return Err(Error("Voice message is too long"));
                }
                packets.push(std::mem::take(&mut current));
            }
        }
        if granule != u64::MAX {
            last_granule = Some(granule);
        }
        ended = flags & 0x04 != 0;
        at = end;
    }
    if !current.is_empty() || packets.len() < 2 {
        return Err(invalid);
    }
    let (channels, pre_skip) = parse_head(&packets[0])?;
    if !packets[1].starts_with(b"OpusTags") {
        return Err(invalid);
    }
    let mut audio = packets.split_off(2);
    let head = packets.swap_remove(0);
    let samples = audio
        .iter()
        .map(|p| opus_packet_samples(p))
        .collect::<Option<Vec<_>>>()
        .ok_or(Error("Voice message contains an invalid Opus packet"))?;
    let total: u64 = samples.iter().map(|s| u64::from(*s)).sum();
    let granule = last_granule.unwrap_or(total);
    // End trimming may shorten the stream; a granule beyond the packets is a lie.
    if granule > total + u64::from(SAMPLE_RATE) {
        return Err(invalid);
    }
    let playable = granule.min(total).saturating_sub(u64::from(pre_skip));
    let duration_ms = u32::try_from(playable * 1000 / u64::from(SAMPLE_RATE)).unwrap_or(u32::MAX);
    audio.shrink_to_fit();
    Ok(OggOpus {
        channels,
        pre_skip,
        head,
        packets: audio,
        samples,
        duration_ms,
    })
}

/// Backend upload rule for voice messages: valid Ogg Opus within the length and
/// byte bounds. Returns the authoritative duration in milliseconds.
pub fn validate_voice_message(bytes: &[u8]) -> Result<u32, Error> {
    if bytes.len() > MAX_VOICE_MESSAGE_BYTES {
        return Err(Error("Voice messages must be 5 MiB or smaller"));
    }
    let parsed = parse_ogg_opus(bytes)?;
    if parsed.duration_ms < MIN_VOICE_MESSAGE_MS {
        return Err(Error("Voice message is too short"));
    }
    if parsed.duration_ms > MAX_VOICE_MESSAGE_MS + DURATION_TOLERANCE_MS {
        return Err(Error("Voice messages can be at most 5 minutes long"));
    }
    Ok(parsed.duration_ms.min(MAX_VOICE_MESSAGE_MS))
}

/// Rewrites a parsed stream into a fresh canonical Ogg file.
pub fn rewrite_ogg(parsed: &OggOpus, serial: u32) -> Vec<u8> {
    let mut writer = OggOpusWriter::new(serial, parsed.channels, parsed.pre_skip);
    for (packet, samples) in parsed.packets.iter().zip(&parsed.samples) {
        writer.push(packet, *samples);
    }
    writer.finish()
}

struct Ebml<'a> {
    bytes: &'a [u8],
    at: usize,
}
const UNKNOWN: u64 = u64::MAX;
impl Ebml<'_> {
    fn vint(&mut self, keep_marker: bool) -> Result<u64, Error> {
        let invalid = Error("Recording is not a valid WebM file");
        let first = *self.bytes.get(self.at).ok_or(invalid)?;
        let length = first.leading_zeros() as usize + 1;
        if length > 8 || (keep_marker && length > 4) {
            return Err(invalid);
        }
        let raw = self.bytes.get(self.at..self.at + length).ok_or(invalid)?;
        self.at += length;
        let mut value = if keep_marker {
            u64::from(first)
        } else {
            u64::from(first) & ((1u64 << (8 - length)) - 1)
        };
        for byte in &raw[1..] {
            value = (value << 8) | u64::from(*byte);
        }
        if !keep_marker && value == (1u64 << (7 * length)) - 1 {
            return Ok(UNKNOWN);
        }
        Ok(value)
    }
}
fn uint(bytes: &[u8]) -> u64 {
    bytes
        .iter()
        .take(8)
        .fold(0, |v, b| (v << 8) | u64::from(*b))
}

/// Extracts the first Opus track of a MediaRecorder WebM/Matroska recording
/// (including live unknown-size segments and clusters) into Ogg Opus.
pub fn webm_to_ogg(bytes: &[u8], serial: u32) -> Result<Vec<u8>, Error> {
    let invalid = Error("Recording is not a valid WebM file");
    if bytes.len() > MAX_RECORDER_BYTES {
        return Err(Error("Recording is too large"));
    }
    // Masters entered rather than skipped; everything else at any level is a leaf.
    const SEGMENT: u64 = 0x1853_8067;
    const CLUSTER: u64 = 0x1F43_B675;
    const TRACKS: u64 = 0x1654_AE6B;
    const ENTRY: u64 = 0xAE;
    const AUDIO: u64 = 0xE1;
    const GROUP: u64 = 0xA0;
    #[derive(Default)]
    struct Entry {
        number: Option<u64>,
        codec: Vec<u8>,
        private: Vec<u8>,
        channels: Option<u8>,
        delay: u64,
    }
    // Selects the first complete Opus entry; later entries are ignored.
    fn commit(
        entry: &mut Option<Entry>,
        track: &mut Option<u64>,
        writer: &mut Option<OggOpusWriter>,
        serial: u32,
    ) -> Result<(), Error> {
        let Some(done) = entry.take() else {
            return Ok(());
        };
        if track.is_some() || done.codec != b"A_OPUS" {
            return Ok(());
        }
        let Some(number) = done.number else {
            return Ok(());
        };
        let head = if done.private.is_empty() {
            // Matroska CodecDelay is in nanoseconds.
            let pre_skip = u16::try_from(done.delay * 48 / 1_000_000).unwrap_or(312);
            opus_head(done.channels.unwrap_or(1), pre_skip)
        } else {
            done.private
        };
        let (channels, pre_skip) = parse_head(&head)?;
        *writer = Some(OggOpusWriter::new(serial, channels, pre_skip));
        *track = Some(number);
        Ok(())
    }
    let mut reader = Ebml { bytes, at: 0 };
    let mut entry = None::<Entry>;
    let mut track = None::<u64>;
    let mut writer = None::<OggOpusWriter>;
    let mut count = 0usize;
    while reader.at < bytes.len() {
        let id = reader.vint(true)?;
        let size = reader.vint(false)?;
        if matches!(id, SEGMENT | CLUSTER | TRACKS | ENTRY | AUDIO | GROUP) {
            if matches!(id, ENTRY | CLUSTER) {
                commit(&mut entry, &mut track, &mut writer, serial)?;
            }
            if id == ENTRY {
                entry = Some(Entry::default());
            }
            continue;
        }
        if size == UNKNOWN {
            return Err(invalid);
        }
        let end = reader
            .at
            .checked_add(usize::try_from(size).map_err(|_| invalid)?)
            .filter(|end| *end <= bytes.len())
            .ok_or(invalid)?;
        let payload = &bytes[reader.at..end];
        reader.at = end;
        if let Some(entry) = entry.as_mut() {
            match id {
                0xD7 => entry.number = Some(uint(payload)),
                0x86 => entry.codec = payload.to_vec(),
                0x63A2 => entry.private = payload.to_vec(),
                0x9F => entry.channels = Some(u8::try_from(uint(payload)).map_err(|_| invalid)?),
                0x56AA => entry.delay = uint(payload),
                _ => {}
            }
        }
        if matches!(id, 0xA3 | 0xA1) {
            commit(&mut entry, &mut track, &mut writer, serial)?;
            let wanted = track.ok_or(Error("Recording has no Opus audio track"))?;
            let mut block = Ebml {
                bytes: payload,
                at: 0,
            };
            let block_track = block.vint(false)?;
            let flags = *payload.get(block.at + 2).ok_or(invalid)?;
            let frame = payload.get(block.at + 3..).ok_or(invalid)?;
            if block_track != wanted {
                continue;
            }
            if flags & 0x06 != 0 {
                return Err(Error("Laced WebM audio is not supported"));
            }
            let samples = opus_packet_samples(frame)
                .ok_or(Error("Recording contains an invalid Opus packet"))?;
            count += 1;
            if count > MAX_PACKETS {
                return Err(Error("Recording is too long"));
            }
            writer.as_mut().ok_or(invalid)?.push(frame, samples);
        }
    }
    let ogg = writer.ok_or(Error("Recording has no Opus audio"))?.finish();
    if count == 0 {
        return Err(Error("Recording is empty"));
    }
    Ok(ogg)
}

fn element(out: &mut Vec<u8>, id: &[u8], payload: &[u8]) {
    out.extend_from_slice(id);
    let size = payload.len() as u64;
    let length = (1..=8).find(|l| size < (1u64 << (7 * l)) - 1).unwrap_or(8);
    let marked = size | (1u64 << (7 * length));
    out.extend_from_slice(&marked.to_be_bytes()[8 - length..]);
    out.extend_from_slice(payload);
}
fn uint_element(out: &mut Vec<u8>, id: &[u8], value: u64) {
    let bytes = value.to_be_bytes();
    let skip = bytes.iter().take(7).take_while(|b| **b == 0).count();
    element(out, id, &bytes[skip..]);
}

/// WebM copy of a parsed stream for browsers whose media element cannot play
/// Ogg (some Safari versions). Packets are copied; nothing is re-encoded.
pub fn ogg_to_webm(parsed: &OggOpus) -> Vec<u8> {
    let mut out = Vec::new();
    let mut header = Vec::new();
    uint_element(&mut header, &[0x42, 0x86], 1);
    uint_element(&mut header, &[0x42, 0xF7], 1);
    uint_element(&mut header, &[0x42, 0xF2], 4);
    uint_element(&mut header, &[0x42, 0xF3], 8);
    element(&mut header, &[0x42, 0x82], b"webm");
    uint_element(&mut header, &[0x42, 0x87], 4);
    uint_element(&mut header, &[0x42, 0x85], 2);
    element(&mut out, &[0x1A, 0x45, 0xDF, 0xA3], &header);

    let mut segment = Vec::new();
    let mut info = Vec::new();
    uint_element(&mut info, &[0x2A, 0xD7, 0xB1], 1_000_000);
    element(&mut info, &[0x4D, 0x80], b"thiscord");
    element(&mut info, &[0x57, 0x41], b"thiscord");
    element(
        &mut info,
        &[0x44, 0x89],
        &f64::from(parsed.duration_ms).to_be_bytes(),
    );
    element(&mut segment, &[0x15, 0x49, 0xA9, 0x66], &info);
    let mut entry = Vec::new();
    uint_element(&mut entry, &[0xD7], 1);
    uint_element(&mut entry, &[0x73, 0xC5], 1);
    uint_element(&mut entry, &[0x83], 2);
    element(&mut entry, &[0x86], b"A_OPUS");
    element(&mut entry, &[0x63, 0xA2], &parsed.head);
    uint_element(
        &mut entry,
        &[0x56, 0xAA],
        u64::from(parsed.pre_skip) * 1_000_000_000 / u64::from(SAMPLE_RATE),
    );
    uint_element(&mut entry, &[0x56, 0xBB], 80_000_000);
    let mut audio = Vec::new();
    element(&mut audio, &[0xB5], &f64::from(SAMPLE_RATE).to_be_bytes());
    uint_element(&mut audio, &[0x9F], u64::from(parsed.channels));
    element(&mut entry, &[0xE1], &audio);
    let mut tracks = Vec::new();
    element(&mut tracks, &[0xAE], &entry);
    element(&mut segment, &[0x16, 0x54, 0xAE, 0x6B], &tracks);

    let mut cluster = Vec::new();
    let mut cluster_start = 0u64;
    for (packet, offset) in parsed.packets.iter().zip(parsed.offsets()) {
        let at = offset * 1000 / u64::from(SAMPLE_RATE);
        if cluster.is_empty() || at - cluster_start > 30_000 {
            if !cluster.is_empty() {
                element(&mut segment, &[0x1F, 0x43, 0xB6, 0x75], &cluster);
                cluster.clear();
            }
            cluster_start = at;
            uint_element(&mut cluster, &[0xE7], at);
        }
        let mut block = vec![0x81];
        block.extend_from_slice(&((at - cluster_start) as i16).to_be_bytes());
        block.push(0x80);
        block.extend_from_slice(packet);
        element(&mut cluster, &[0xA3], &block);
    }
    if !cluster.is_empty() {
        element(&mut segment, &[0x1F, 0x43, 0xB6, 0x75], &cluster);
    }
    element(&mut out, &[0x18, 0x53, 0x80, 0x67], &segment);
    out
}

/// Compact waveform from per-frame RMS levels (linear 0–1): the loudest frame of
/// each bar on a 50 dB scale, normalized so the loudest bar is full height.
pub fn waveform(levels: &[f32]) -> Vec<u8> {
    if levels.is_empty() {
        return vec![0; WAVEFORM_BARS];
    }
    let bars: Vec<f32> = (0..WAVEFORM_BARS)
        .map(|bar| {
            let start = bar * levels.len() / WAVEFORM_BARS;
            let end = ((bar + 1) * levels.len() / WAVEFORM_BARS).max(start + 1);
            let peak = levels[start.min(levels.len() - 1)..end.min(levels.len())]
                .iter()
                .fold(0f32, |a, b| {
                    a.max(if b.is_finite() { b.abs() } else { 0.0 })
                });
            ((20.0 * peak.max(1e-5).log10() + 50.0) / 50.0).clamp(0.0, 1.0)
        })
        .collect();
    let loudest = bars.iter().fold(0f32, |a, b| a.max(*b));
    let scale = if loudest > 0.05 { 1.0 / loudest } else { 1.0 };
    bars.into_iter()
        .map(|v| (v * scale * 255.0).round().clamp(0.0, 255.0) as u8)
        .collect()
}
pub fn encode_waveform(bars: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bars)
}
/// Exactly [`WAVEFORM_BARS`] bytes, or None.
pub fn decode_waveform(value: &str) -> Option<Vec<u8>> {
    if value.len() > 128 {
        return None;
    }
    URL_SAFE_NO_PAD
        .decode(value)
        .ok()
        .filter(|bars| bars.len() == WAVEFORM_BARS)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 20 ms CELT fullband mono frames (config 31, code 0) with dummy payloads.
    fn packet(len: usize) -> Vec<u8> {
        let mut p = vec![0xF8];
        p.extend(std::iter::repeat_n(0x55, len));
        p
    }

    fn recording(seconds: u32) -> Vec<u8> {
        let mut writer = OggOpusWriter::new(7, 1, 312);
        for i in 0..seconds * 50 {
            writer.push(&packet(60 + (i as usize % 400)), 960);
        }
        writer.finish()
    }

    #[test]
    fn toc_durations_follow_rfc6716() {
        assert_eq!(opus_packet_samples(&[0xF8]), Some(960));
        assert_eq!(opus_packet_samples(&[0x08]), Some(960)); // SILK NB 20 ms
        assert_eq!(opus_packet_samples(&[0x18]), Some(2880)); // SILK NB 60 ms
        assert_eq!(opus_packet_samples(&[0x80]), Some(120)); // CELT 2.5 ms
        assert_eq!(opus_packet_samples(&[0xF9]), Some(1920)); // two frames
        assert_eq!(opus_packet_samples(&[0xFB, 0x03]), Some(2880)); // code 3, 3 frames
        assert_eq!(opus_packet_samples(&[0xFB, 0x00]), None);
        assert_eq!(opus_packet_samples(&[0x1B, 0x03]), None); // 180 ms
        assert_eq!(opus_packet_samples(&[0xFB]), None);
        assert_eq!(opus_packet_samples(&[]), None);
    }

    #[test]
    fn ogg_round_trip_preserves_packets_and_duration() {
        let bytes = recording(3);
        let parsed = parse_ogg_opus(&bytes).unwrap();
        assert_eq!((parsed.channels, parsed.pre_skip), (1, 312));
        assert_eq!(parsed.packets.len(), 150);
        assert_eq!(parsed.packets[7], packet(67));
        assert_eq!(parsed.duration_ms, (150 * 960 - 312) * 1000 / 48_000);
        assert_eq!(validate_voice_message(&bytes).unwrap(), parsed.duration_ms);
        assert_eq!(rewrite_ogg(&parsed, 7), bytes);
        // Packets longer than 255 bytes span lacing values and still round-trip.
        let mut writer = OggOpusWriter::new(1, 2, 0);
        writer.push(&packet(1000), 960);
        writer.push(&packet(254), 960);
        let parsed = parse_ogg_opus(&writer.finish()).unwrap();
        assert_eq!(parsed.packets, vec![packet(1000), packet(254)]);
    }

    #[test]
    fn corrupted_chained_truncated_and_oversized_files_are_rejected() {
        let bytes = recording(2);
        let mut flipped = bytes.clone();
        let last = flipped.len() - 1;
        flipped[last] ^= 1;
        assert!(parse_ogg_opus(&flipped).is_err());
        assert!(parse_ogg_opus(&bytes[..bytes.len() - 3]).is_err());
        let mut trailing = bytes.clone();
        trailing.extend_from_slice(b"junk");
        assert!(parse_ogg_opus(&trailing).is_err());
        let mut chained = bytes.clone();
        chained.extend_from_slice(&recording(1));
        assert!(parse_ogg_opus(&chained).is_err());
        assert!(parse_ogg_opus(b"<svg onload=alert(1)>").is_err());
        assert!(parse_ogg_opus(b"").is_err());
        assert!(validate_voice_message(&recording(0)).is_err());
        assert!(validate_voice_message(&vec![0; MAX_VOICE_MESSAGE_BYTES + 1]).is_err());
        // 302 s exceeds the limit and tolerance.
        let mut writer = OggOpusWriter::new(7, 1, 0);
        for _ in 0..302 * 50 {
            writer.push(&packet(2), 960);
        }
        assert!(validate_voice_message(&writer.finish()).is_err());
        // A granule claiming far more audio than its packets is rejected.
        let mut writer = OggOpusWriter::new(7, 1, 0);
        writer.push(&packet(10), 960);
        writer.granule = 48_000 * 600;
        assert!(parse_ogg_opus(&writer.finish()).is_err());
    }

    #[test]
    fn surround_and_invalid_packets_are_rejected() {
        let mut head = opus_head(6, 0);
        head[18] = 1;
        assert!(parse_head(&head).is_err());
        let mut writer = OggOpusWriter::new(1, 1, 0);
        writer.push(&[0xFB, 0x00], 960);
        assert!(parse_ogg_opus(&writer.finish()).is_err());
    }

    /// A MediaRecorder-shaped file: unknown-size segment/cluster, CodecPrivate.
    fn webm(packets: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80];
        out.extend_from_slice(&[
            0x18, 0x53, 0x80, 0x67, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        let mut entry = Vec::new();
        uint_element(&mut entry, &[0xD7], 1);
        element(&mut entry, &[0x86], b"A_OPUS");
        element(&mut entry, &[0x63, 0xA2], &opus_head(1, 312));
        let mut tracks = Vec::new();
        element(&mut tracks, &[0xAE], &entry);
        element(&mut out, &[0x16, 0x54, 0xAE, 0x6B], &tracks);
        out.extend_from_slice(&[
            0x1F, 0x43, 0xB6, 0x75, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF,
        ]);
        uint_element(&mut out, &[0xE7], 0);
        for (i, p) in packets.iter().enumerate() {
            let mut block = vec![0x81];
            block.extend_from_slice(&((i * 20) as i16).to_be_bytes());
            block.push(0x80);
            block.extend_from_slice(p);
            element(&mut out, &[0xA3], &block);
        }
        out
    }

    #[test]
    fn webm_recordings_remux_to_ogg_and_back() {
        let packets: Vec<_> = (0..100).map(|i| packet(40 + i)).collect();
        let ogg = webm_to_ogg(&webm(&packets), 9).unwrap();
        let parsed = parse_ogg_opus(&ogg).unwrap();
        assert_eq!(parsed.packets, packets);
        assert_eq!(parsed.pre_skip, 312);
        assert!(validate_voice_message(&ogg).is_ok());
        let again = webm_to_ogg(&ogg_to_webm(&parsed), 9).unwrap();
        assert_eq!(parse_ogg_opus(&again).unwrap().packets, packets);
        assert!(webm_to_ogg(b"\x1a\x45\xdf\xa3", 1).is_err());
        assert!(webm_to_ogg(&webm(&[]), 1).is_err());
        let mut laced = webm(&packets[..1]);
        let flags = laced.len() - packets[0].len() - 1;
        laced[flags] |= 0x02;
        assert!(webm_to_ogg(&laced, 1).is_err());
    }

    #[test]
    fn waveforms_are_bounded_normalized_and_validated() {
        let bars = waveform(&[0.0, 0.01, 0.1, 1.0]);
        assert_eq!(bars.len(), WAVEFORM_BARS);
        assert_eq!(*bars.iter().max().unwrap(), 255);
        assert_eq!(waveform(&[]), vec![0; WAVEFORM_BARS]);
        let long: Vec<f32> = (0..15_000).map(|i| (i % 50) as f32 / 50.0).collect();
        assert_eq!(waveform(&long).len(), WAVEFORM_BARS);
        assert_eq!(waveform(&[f32::NAN, f32::INFINITY]).len(), WAVEFORM_BARS);
        let encoded = encode_waveform(&bars);
        assert_eq!(decode_waveform(&encoded).unwrap(), bars);
        assert!(decode_waveform(&encode_waveform(&[1, 2, 3])).is_none());
        assert!(decode_waveform("not base64!").is_none());
        assert!(decode_waveform(&"A".repeat(4000)).is_none());
    }
}
