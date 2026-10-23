//! FFmpeg produces paced H.264/Opus on private loopback sockets. Only bounded
//! encoded RTP crosses into Rust; WebRTC encrypts it before it leaves the host.
use super::*;
use media::{MediaTool, Processes, Source};
use rtc::{rtp, shared::marshal::Unmarshal};
use std::process::Stdio;
use thiscord_shared::voice::MediaKind;
use tokio::{net::UdpSocket, task::JoinHandle};

type Result<T> = std::result::Result<T, &'static str>;

pub(super) struct Packet {
    pub kind: MediaKind,
    pub rtp: rtp::Packet,
    pub arrived: Instant,
}

pub(super) struct Player {
    pub packets: mpsc::Receiver<Packet>,
    cancel: watch::Sender<bool>,
    job: Option<JoinHandle<Result<()>>>,
}
impl Player {
    pub fn start(source: Source) -> Self {
        let (tx, packets) = mpsc::channel(64);
        let (cancel, mut cancelled) = watch::channel(false);
        let job = tokio::spawn(async move {
            let mut processes = Processes::default();
            let result = tokio::select! { biased;
                _ = async { let _ = cancelled.wait_for(|v| *v).await; } => Ok(()),
                result = timeout(media::MAX_PLAYBACK, run(&mut processes, source, tx)) => {
                    result.unwrap_or(Err("Video exceeded the six-hour limit"))
                },
            };
            processes.stop().await;
            result
        });
        Self {
            packets,
            cancel,
            job: Some(job),
        }
    }

    pub async fn finish(&mut self) -> Result<()> {
        let Some(mut job) = self.job.take() else {
            return Ok(());
        };
        match timeout(Duration::from_secs(3), &mut job).await {
            Ok(result) => result.unwrap_or(Err("Video worker failed")),
            Err(_) => {
                job.abort();
                let _ = job.await;
                Err("Video process cleanup timed out")
            }
        }
    }

    pub async fn stop(&mut self) {
        self.cancel.send_replace(true);
        self.packets.close();
        let _ = self.finish().await;
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
    }
}

async fn run(
    processes: &mut Processes,
    source: Source,
    packets: mpsc::Sender<Packet>,
) -> Result<()> {
    let video = UdpSocket::bind("127.0.0.1:0")
        .await
        .map_err(|_| "Cannot open video pipe")?;
    let audio = UdpSocket::bind("127.0.0.1:0")
        .await
        .map_err(|_| "Cannot open audio pipe")?;
    let video_ssrc = rand::random::<u32>() & 0x7fff_ffff;
    let audio_ssrc = rand::random::<u32>() & 0x7fff_ffff;
    let mut ffmpeg = media::command("ffmpeg");
    ffmpeg.stdout(Stdio::null()).args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-nostdin",
        "-threads",
        "1",
        "-re",
    ]);
    match source {
        Source::File(path) => {
            let path = tokio::task::spawn_blocking(move || path.canonicalize())
                .await
                .map_err(|_| "Media file lookup failed")?
                .map_err(|_| "Media file unavailable")?;
            if !path.is_file() {
                return Err("Media input must be a regular file");
            }
            ffmpeg
                .args(["-protocol_whitelist", "file,pipe", "-i"])
                .arg(path);
        }
        Source::Youtube(url) => {
            processes.downloader = Some(
                media::downloader(&url, true)
                    .spawn()
                    .map_err(|e| MediaTool::YoutubeDownloader.spawn_error(e))?,
            );
            ffmpeg
                .stdin(Stdio::piped())
                .args(["-protocol_whitelist", "pipe", "-i", "pipe:0"]);
        }
    }
    // One decoder timeline drives both outputs. Baseline H.264 with repeated
    // parameter sets and a one-second IDR permits viewers to join mid-video.
    ffmpeg.args([
        "-map", "0:v:0", "-an", "-sn", "-dn", "-threads", "1",
        "-vf", "scale=w='min(1280,iw)':h='min(720,ih)':force_original_aspect_ratio=decrease:force_divisible_by=2,fps=30,format=yuv420p",
        "-c:v", "libx264", "-preset", "veryfast", "-tune", "zerolatency",
        "-profile:v", "baseline", "-level:v", "5.2", "-b:v", "2000k", "-maxrate", "2500k", "-bufsize", "500k",
        "-g", "30", "-keyint_min", "30", "-sc_threshold", "0", "-x264-params", "repeat-headers=1",
        "-f", "rtp", "-rtpflags", "skip_rtcp", "-payload_type",
    ]).arg(MediaKind::ScreenVideo.payload_type().to_string())
        .arg("-ssrc").arg(video_ssrc.to_string())
        .arg(format!("rtp://{}?pkt_size=1200", video.local_addr().map_err(|_| "Cannot open video pipe")?));
    ffmpeg
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-sn",
            "-dn",
            "-threads",
            "1",
            "-c:a",
            "libopus",
            "-b:a",
            "96k",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-frame_duration",
            "20",
            "-f",
            "rtp",
            "-rtpflags",
            "skip_rtcp",
            "-payload_type",
        ])
        .arg(MediaKind::SystemAudio.payload_type().to_string())
        .arg("-ssrc")
        .arg(audio_ssrc.to_string())
        .arg(format!(
            "rtp://{}?pkt_size=1200",
            audio.local_addr().map_err(|_| "Cannot open audio pipe")?
        ));
    processes.decoder = Some(
        ffmpeg
            .spawn()
            .map_err(|e| MediaTool::Decoder.spawn_error(e))?,
    );
    let input = processes.decoder.as_mut().and_then(|c| c.stdin.take());
    let output = processes.downloader.as_mut().and_then(|c| c.stdout.take());
    let copy = async move {
        if let (Some(output), Some(input)) = (output, input) {
            media::transfer(output, input).await
        } else {
            Ok(())
        }
    };
    tokio::pin!(copy);
    let mut copied = false;
    let mut video_buffer = [0; 1500];
    let mut audio_buffer = [0; 1500];
    let mut seen = [false; 2];
    let mut last_packet = Instant::now();
    let mut check = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            result = &mut copy, if !copied => { result?; copied = true; },
            result = video.recv(&mut video_buffer) => {
                let n = result.map_err(|_| "Video pipe failed")?;
                forward(&video_buffer[..n], MediaKind::ScreenVideo, video_ssrc, &packets).await?;
                seen[0] = true; last_packet = Instant::now();
            },
            result = audio.recv(&mut audio_buffer) => {
                let n = result.map_err(|_| "Audio pipe failed")?;
                forward(&audio_buffer[..n], MediaKind::SystemAudio, audio_ssrc, &packets).await?;
                seen[1] = true; last_packet = Instant::now();
            },
            _ = check.tick() => {
                if let Some(downloader) = processes.downloader.as_mut()
                    && downloader.try_wait().map_err(|_| "Downloader wait failed")?.is_some_and(|s| !s.success()) {
                    return Err("yt-dlp could not retrieve video; check whether YouTube permits access from this host");
                }
                if let Some(status) = processes.decoder.as_mut().ok_or("Missing decoder")?.try_wait().map_err(|_| "Video process wait failed")? {
                    if !status.success() { return Err("ffmpeg could not stream video; the input needs video and audio, and FFmpeg needs libx264 and libopus"); }
                    // Let the last datagrams drain before closing the bounded queue.
                    if last_packet.elapsed() >= Duration::from_millis(100) {
                        if !seen.into_iter().all(|v| v) { return Err("Media must contain both video and audio"); }
                        if let Some(downloader) = processes.downloader.as_mut() {
                            media::wait(downloader, "yt-dlp could not retrieve video").await?;
                        }
                        return Ok(());
                    }
                }
                if last_packet.elapsed() > Duration::from_secs(30) { return Err("Video decoder stalled"); }
            },
        }
    }
}

async fn forward(
    data: &[u8],
    kind: MediaKind,
    ssrc: u32,
    packets: &mpsc::Sender<Packet>,
) -> Result<()> {
    let packet =
        rtp::Packet::unmarshal(&mut &data[..]).map_err(|_| "Invalid encoded video packet")?;
    if packet.header.version != 2
        || packet.header.ssrc != ssrc
        || packet.header.payload_type != kind.payload_type()
        || packet.payload.is_empty()
        || packet.payload.len() > 1200
    {
        return Err("Unexpected encoded video packet");
    }
    timeout(
        Duration::from_millis(200),
        packets.send(Packet {
            kind,
            rtp: packet,
            arrived: Instant::now(),
        }),
    )
    .await
    .map_err(|_| "Video sender fell behind")?
    .map_err(|_| "Playback stopped")
}

/// Preserve sequence/clock continuity under each SRTP SSRC across replacement.
pub(super) struct Clock {
    sequence: u16,
    timestamp: u32,
    start: Instant,
    source: Option<(u32, u64, u16, u16)>,
    last: u64,
}
impl Clock {
    pub fn new() -> Self {
        Self {
            sequence: rand::random(),
            timestamp: rand::random(),
            start: Instant::now(),
            source: None,
            last: 0,
        }
    }
    pub fn restart(&mut self) {
        self.source = None;
    }
    pub fn packet(&mut self, mut packet: rtp::Packet, kind: MediaKind) -> rtp::Packet {
        let (source, base, source_sequence, sequence) = *self.source.get_or_insert_with(|| {
            (
                packet.header.timestamp,
                ((self.start.elapsed().as_secs_f64() * kind.clock_rate() as f64) as u64)
                    .max(self.last.saturating_add(1)),
                packet.header.sequence_number,
                self.sequence.wrapping_add(1),
            )
        });
        self.last = base + u64::from(packet.header.timestamp.wrapping_sub(source));
        // Preserve gaps on the loopback leg so receivers discard a damaged
        // access unit instead of silently joining nonadjacent H.264 fragments.
        self.sequence =
            sequence.wrapping_add(packet.header.sequence_number.wrapping_sub(source_sequence));
        packet.header = rtp::header::Header {
            version: 2,
            payload_type: kind.payload_type(),
            ssrc: kind.publisher_ssrc(),
            sequence_number: self.sequence,
            timestamp: self.timestamp.wrapping_add(self.last as u32),
            marker: packet.header.marker,
            ..Default::default()
        };
        packet
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn video_clock_keeps_fragments_together_and_never_reuses_sequence_on_replace() {
        let mut clock = Clock::new();
        clock.sequence = u16::MAX - 1;
        let packet = |timestamp, sequence_number| rtp::Packet {
            header: rtp::header::Header {
                timestamp,
                sequence_number,
                marker: true,
                ..Default::default()
            },
            payload: vec![0x65, 1].into(),
        };
        let first = clock.packet(packet(u32::MAX - 10, 100), MediaKind::ScreenVideo);
        let fragment = clock.packet(packet(u32::MAX - 10, 101), MediaKind::ScreenVideo);
        let next = clock.packet(packet(2989, 104), MediaKind::ScreenVideo);
        assert_eq!(first.header.timestamp, fragment.header.timestamp);
        assert_eq!(
            next.header.timestamp.wrapping_sub(first.header.timestamp),
            3000
        );
        assert_eq!(fragment.header.sequence_number, 0);
        assert_eq!(
            next.header.sequence_number, 3,
            "lost input packets stay visible"
        );
        clock.restart();
        let replaced = clock.packet(packet(5, 50), MediaKind::ScreenVideo);
        assert_eq!(
            replaced.header.sequence_number,
            next.header.sequence_number.wrapping_add(1)
        );
        assert!(
            replaced
                .header
                .timestamp
                .wrapping_sub(next.header.timestamp)
                > 0
        );
        assert_eq!(
            replaced.header.ssrc,
            MediaKind::ScreenVideo.publisher_ssrc()
        );
    }

    pub struct Clip(pub std::path::PathBuf);
    impl Clip {
        pub async fn new(seconds: u8) -> Self {
            let path = std::env::temp_dir()
                .join(format!("thiscord-bot-video-{}.mkv", uuid::Uuid::new_v4()));
            let status = tokio::process::Command::new("ffmpeg")
                .args([
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-nostdin",
                    "-f",
                    "lavfi",
                    "-i",
                    "testsrc2=size=320x180:rate=30",
                    "-f",
                    "lavfi",
                    "-i",
                    "sine=frequency=440:sample_rate=48000",
                    "-t",
                ])
                .arg(seconds.to_string())
                .args([
                    "-threads",
                    "1",
                    "-c:v",
                    "libx264",
                    "-preset",
                    "ultrafast",
                    "-c:a",
                    "libopus",
                ])
                .arg(&path)
                .status()
                .await
                .unwrap();
            assert!(status.success());
            Self(path)
        }
    }
    impl Drop for Clip {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg with libx264 and libopus on PATH"]
    async fn ffmpeg_video_emits_recoverable_h264_and_opus_and_finishes() {
        let clip = Clip::new(2).await;
        let mut player = Player::start(Source::File(clip.0.clone()));
        let mut assembler = thiscord_shared::h264::Assembler::default();
        let mut recoveries = 0;
        let mut frames = 0;
        let mut audio = 0;
        let mut decoder = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
        let mut pcm = [0_i16; 1920];
        timeout(Duration::from_secs(15), async {
            while let Some(packet) = player.packets.recv().await {
                let p = packet.rtp;
                match packet.kind {
                    MediaKind::ScreenVideo => {
                        if let Some(frame) = assembler.push(
                            p.header.timestamp,
                            p.header.sequence_number,
                            p.header.marker,
                            &p.payload,
                        ) {
                            frames += 1;
                            if thiscord_shared::h264::recovery_frame(&frame) {
                                assert!(thiscord_shared::h264::bounded_parameter_sets(&frame));
                                recoveries += 1;
                            }
                        }
                    }
                    MediaKind::SystemAudio => {
                        assert_eq!(decoder.decode(&p.payload, &mut pcm, false).unwrap(), 960);
                        audio += 1;
                    }
                    _ => panic!("unexpected stream"),
                }
            }
            player.finish().await.unwrap();
        })
        .await
        .unwrap();
        assert!(
            frames >= 50 && recoveries >= 2 && audio >= 90,
            "frames={frames} recoveries={recoveries} audio={audio}"
        );
    }
}
