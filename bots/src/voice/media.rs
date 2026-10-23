use super::*;
use bytes::Bytes;
use std::{path::PathBuf, process::Stdio};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::{Child, ChildStdin, Command},
    task::JoinHandle,
};

const SAMPLES: usize = 960 * 2;
const MAX_PLAYBACK: Duration = Duration::from_secs(6 * 60 * 60);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
type Result<T> = std::result::Result<T, &'static str>;

pub(super) enum Source {
    File(PathBuf),
    Youtube(String),
}

/// Canonicalize to a single video URL. Never pass arbitrary caller-controlled
/// queries, fragments, credentials, redirects or extractor names to yt-dlp.
pub(super) fn youtube_url(input: &str) -> std::result::Result<String, Error> {
    let invalid = || Error::message("Use an HTTPS YouTube video URL");
    if input.len() > 2048 || input.chars().any(char::is_control) {
        return Err(invalid());
    }
    let url = url::Url::parse(input).map_err(|_| invalid())?;
    if url.scheme() != "https"
        || !url.username().is_empty()
        || url.password().is_some()
        || url.port().is_some()
    {
        return Err(invalid());
    }
    let id = match url.host_str() {
        Some("youtube.com" | "www.youtube.com" | "m.youtube.com" | "music.youtube.com") => {
            if url.path() == "/watch" {
                let ids: Vec<_> = url.query_pairs().filter(|(key, _)| key == "v").collect();
                if ids.len() != 1 {
                    return Err(invalid());
                }
                ids[0].1.to_string()
            } else if let Some(id) = url.path().strip_prefix("/shorts/") {
                id.to_owned()
            } else {
                return Err(invalid());
            }
        }
        Some("youtu.be") => url.path().strip_prefix('/').ok_or_else(invalid)?.to_owned(),
        _ => return Err(invalid()),
    };
    if id.len() != 11
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
    {
        return Err(invalid());
    }
    Ok(format!("https://www.youtube.com/watch?v={id}"))
}

pub(super) struct Player {
    pub(super) frames: mpsc::Receiver<Bytes>,
    cancel: watch::Sender<bool>,
    job: Option<JoinHandle<Result<()>>>,
}
impl Player {
    pub(super) fn start(source: Source) -> Self {
        let (tx, frames) = mpsc::channel(3);
        let (cancel, mut cancelled) = watch::channel(false);
        let job = tokio::spawn(async move {
            let mut processes = Processes::default();
            let result = tokio::select! { biased;
                _ = async { let _ = cancelled.wait_for(|value| *value).await; } => Ok(()),
                result = timeout(MAX_PLAYBACK, processes.run(source, tx)) => {
                    result.unwrap_or(Err("Playback exceeded the six-hour limit"))
                },
            };
            processes.stop().await;
            result
        });
        Self {
            frames,
            cancel,
            job: Some(job),
        }
    }

    pub(super) async fn finish(&mut self) -> Result<()> {
        let Some(mut job) = self.job.take() else {
            return Ok(());
        };
        match timeout(Duration::from_secs(3), &mut job).await {
            Ok(result) => result.unwrap_or(Err("Media worker failed")),
            Err(_) => {
                job.abort();
                let _ = job.await;
                Err("Media process cleanup timed out")
            }
        }
    }

    pub(super) async fn stop(&mut self) {
        self.cancel.send_replace(true);
        self.frames.close();
        let _ = self.finish().await;
    }
}
impl Drop for Player {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
        // Do not abort here: the worker owns bounded kill + wait cleanup. Its
        // children additionally have kill_on_drop for panic/runtime shutdown.
    }
}

#[derive(Default)]
struct Processes {
    decoder: Option<Child>,
    downloader: Option<Child>,
}

fn command(program: &str) -> Command {
    let mut command = Command::new(program);
    command.env_clear();
    // No bot credentials, proxy credentials, Python injection hooks or user
    // configuration are inherited. PATH is the explicitly trusted tool source.
    for name in ["PATH", "SystemRoot", "WINDIR", "TEMP", "TMP", "TMPDIR"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command
}

impl Processes {
    async fn run(&mut self, source: Source, frames: mpsc::Sender<Bytes>) -> Result<()> {
        let mut ffmpeg = command("ffmpeg");
        ffmpeg.args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-threads",
            "1",
        ]);
        let youtube = matches!(&source, Source::Youtube(_));
        match source {
            Source::File(path) => {
                // Resolve before launching: a leading '-' or a URL-like filename
                // is always a local absolute path after canonicalization.
                let path = tokio::task::spawn_blocking(move || {
                    let path = path.canonicalize().map_err(|_| "Media file unavailable")?;
                    if !path.is_file() {
                        return Err("Media input must be a regular file");
                    }
                    Ok(path)
                })
                .await
                .map_err(|_| "Media file lookup failed")??;
                ffmpeg
                    .args(["-protocol_whitelist", "file,pipe", "-i"])
                    .arg(path);
            }
            Source::Youtube(url) => {
                let mut downloader = command("yt-dlp");
                // Direct HTTPS formats only: no HLS/DASH or external downloader
                // subprocesses. Ignore user config/plugins, cookies and proxies.
                downloader.args([
                    "--ignore-config",
                    "--no-plugin-dirs",
                    "--no-js-runtimes",
                    "--no-remote-components",
                    "--no-cache-dir",
                    "--no-playlist",
                    "--no-progress",
                    "--quiet",
                    "--no-warnings",
                    "--proxy",
                    "",
                    "--socket-timeout",
                    "15",
                    "--retries",
                    "2",
                    "--fragment-retries",
                    "0",
                    "--downloader",
                    "native",
                    "--fixup",
                    "never",
                    "--use-extractors",
                    "Youtube",
                    "--match-filters",
                    "!is_live & duration <= 21600",
                    "--max-filesize",
                    "256M",
                    "-f",
                    "bestaudio[protocol=https]/best[protocol=https]",
                    "-o",
                    "-",
                    "--",
                    &url,
                ]);
                self.downloader = Some(downloader.spawn().map_err(|_| "Could not start yt-dlp")?);
                ffmpeg
                    .stdin(Stdio::piped())
                    .args(["-protocol_whitelist", "pipe", "-i", "pipe:0"]);
            }
        }
        ffmpeg.args([
            "-map",
            "0:a:0",
            "-vn",
            "-sn",
            "-dn",
            "-threads",
            "1",
            "-ac",
            "2",
            "-ar",
            "48000",
            "-f",
            "s16le",
            "-acodec",
            "pcm_s16le",
            "pipe:1",
        ]);
        self.decoder = Some(ffmpeg.spawn().map_err(|_| "Could not start ffmpeg")?);
        let output = self
            .decoder
            .as_mut()
            .and_then(|child| child.stdout.take())
            .ok_or("Missing decoder output")?;
        if youtube {
            let download = self
                .downloader
                .as_mut()
                .and_then(|child| child.stdout.take())
                .ok_or("Missing downloader output")?;
            let input = self
                .decoder
                .as_mut()
                .and_then(|child| child.stdin.take())
                .ok_or("Missing decoder input")?;
            tokio::try_join!(encode(output, frames), transfer(download, input))?;
        } else {
            encode(output, frames).await?;
        }
        wait(
            self.decoder.as_mut().ok_or("Missing decoder process")?,
            "ffmpeg could not decode media",
        )
        .await?;
        if let Some(downloader) = self.downloader.as_mut() {
            wait(downloader, "yt-dlp could not retrieve media").await?;
        }
        Ok(())
    }

    async fn stop(&mut self) -> bool {
        // Start both kills before awaiting either process, then reap together.
        for child in [&mut self.decoder, &mut self.downloader]
            .into_iter()
            .flatten()
        {
            let _ = child.start_kill();
        }
        async fn reap(child: &mut Option<Child>) -> bool {
            if let Some(mut child) = child.take() {
                return matches!(timeout(IO_TIMEOUT, child.wait()).await, Ok(Ok(_)));
            }
            true
        }
        let (decoder, downloader) =
            tokio::join!(reap(&mut self.decoder), reap(&mut self.downloader));
        decoder && downloader
    }
}

/// Own both pipe ends: completing this future must close the decoder's OS stdin
/// handle, including while its sibling decode future is still being polled.
/// `shutdown()` alone only flushes Tokio's ChildStdin and does not deliver EOF.
async fn transfer(mut source: impl AsyncRead + Unpin, mut input: ChildStdin) -> Result<()> {
    // Bounded copy backpressures download -> decoder -> three frames -> RTP.
    tokio::io::copy(&mut source, &mut input)
        .await
        .map_err(|_| "Media transfer failed")?;
    input
        .shutdown()
        .await
        .map_err(|_| "Media transfer failed")?;
    drop(input);
    Ok(())
}

async fn wait(child: &mut Child, failed: &'static str) -> Result<()> {
    let status = timeout(IO_TIMEOUT, child.wait())
        .await
        .map_err(|_| "Media process did not exit")?
        .map_err(|_| "Media process wait failed")?;
    if !status.success() {
        return Err(failed);
    }
    Ok(())
}

pub(super) async fn encode(
    mut input: impl AsyncRead + Unpin,
    frames: mpsc::Sender<Bytes>,
) -> Result<()> {
    let mut encoder = opus::Encoder::new(48_000, opus::Channels::Stereo, opus::Application::Audio)
        .map_err(|_| "Opus encoder unavailable")?;
    encoder
        .set_bitrate(opus::Bitrate::Bits(96_000))
        .map_err(|_| "Opus configuration failed")?;
    // No decoder/FEC/DRED history crosses playback sessions. Bound output below
    // the SFU ingress payload maximum and the network MTU.
    let mut output = [0; thiscord_shared::rtp_red::MAX_PAYLOAD];
    let mut pcm = [0_i16; SAMPLES];
    let mut raw = [0; SAMPLES * 2];
    let mut emitted = false;
    loop {
        let mut used = 0;
        while used < raw.len() {
            let count = timeout(READ_TIMEOUT, input.read(&mut raw[used..]))
                .await
                .map_err(|_| "Media decoder stalled")?
                .map_err(|_| "Media decoder read failed")?;
            if count == 0 {
                break;
            }
            used += count;
        }
        if used == 0 {
            return if emitted {
                Ok(())
            } else {
                Err("Media contained no audio")
            };
        }
        if used % 4 != 0 {
            return Err("Media decoder returned incomplete stereo PCM");
        }
        raw[used..].fill(0);
        for (sample, bytes) in pcm.iter_mut().zip(raw.as_chunks::<2>().0) {
            *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
        }
        let size = encoder
            .encode(&pcm, &mut output)
            .map_err(|_| "Opus encoding failed")?;
        frames
            .send(Bytes::copy_from_slice(&output[..size]))
            .await
            .map_err(|_| "Playback stopped")?;
        emitted = true;
        if used < raw.len() {
            return Ok(());
        }
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    fn stdin_decoder() -> Child {
        command("ffmpeg")
            .stdin(Stdio::piped())
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-nostdin",
                "-threads",
                "1",
                "-protocol_whitelist",
                "pipe",
                "-f",
                "s16le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-i",
                "pipe:0",
                "-map",
                "0:a:0",
                "-vn",
                "-sn",
                "-dn",
                "-threads",
                "1",
                "-ac",
                "2",
                "-ar",
                "48000",
                "-f",
                "s16le",
                "-acodec",
                "pcm_s16le",
                "pipe:1",
            ])
            .spawn()
            .expect("ffmpeg must be installed to run this ignored test")
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg on PATH"]
    async fn ffmpeg_stdin_transfer_reaches_natural_eof() {
        let mut decoder = stdin_decoder();
        let input = decoder.stdin.take().unwrap();
        let output = decoder.stdout.take().unwrap();
        let raw = vec![0_u8; SAMPLES * 2 * 3];
        let (tx, mut rx) = mpsc::channel::<Bytes>(1);
        let drained = async {
            let mut count = 0;
            let mut opus = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
            while let Some(frame) = rx.recv().await {
                assert_eq!(opus.decode(&frame, &mut [0; SAMPLES], false).unwrap(), 960);
                count += 1;
            }
            count
        };
        // This is the same owned transfer helper and same concurrent encoding
        // path used by YouTube, with finite PCM replacing network yt-dlp output.
        let work = async {
            let (pipeline, count) = tokio::join!(
                async { tokio::try_join!(encode(output, tx), transfer(raw.as_slice(), input)) },
                drained
            );
            pipeline.unwrap();
            assert_eq!(count, 3);
            wait(&mut decoder, "decoder failed").await.unwrap();
            assert!(decoder.try_wait().unwrap().unwrap().success());
        };
        let result = timeout(Duration::from_secs(3), work).await;
        if result.is_err() {
            let _ = decoder.kill().await;
        }
        assert!(
            result.is_ok(),
            "finite stdin did not reach EOF within three seconds"
        );
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg on PATH"]
    async fn ffmpeg_borrowed_stdin_reproduces_pre_fix_stall() {
        let mut decoder = stdin_decoder();
        let mut input = decoder.stdin.take().unwrap();
        let mut output = decoder.stdout.take().unwrap();
        let raw = vec![0_u8; SAMPLES * 2 * 3];
        let (tx, mut rx) = mpsc::channel(3);
        let old_transfer = async {
            let mut source = raw.as_slice();
            tokio::io::copy(&mut source, &mut input).await.unwrap();
            input.shutdown().await.unwrap();
            // Deliberately retain the borrowed OS pipe, reproducing the old bug.
        };
        assert!(
            timeout(Duration::from_secs(1), async {
                let _ = tokio::join!(encode(&mut output, tx), old_transfer, async {
                    while rx.recv().await.is_some() {}
                });
            })
            .await
            .is_err(),
            "the negative control unexpectedly reached EOF"
        );
        assert!(decoder.try_wait().unwrap().is_none());
        drop(input);
        // Closing that same handle unblocks the child without killing it.
        timeout(
            Duration::from_secs(3),
            tokio::io::copy(&mut output, &mut tokio::io::sink()),
        )
        .await
        .unwrap()
        .unwrap();
        wait(&mut decoder, "decoder failed after stdin closed")
            .await
            .unwrap();
    }

    #[test]
    fn youtube_inputs_are_canonical_and_reject_url_injection() {
        for input in [
            "https://youtu.be/dQw4w9WgXcQ?t=20",
            "https://www.youtube.com/watch?v=dQw4w9WgXcQ&list=discard",
            "https://m.youtube.com/shorts/dQw4w9WgXcQ",
        ] {
            assert_eq!(
                youtube_url(input).unwrap(),
                "https://www.youtube.com/watch?v=dQw4w9WgXcQ"
            );
        }
        for input in [
            "--exec=anything",
            "file:///secret",
            "http://youtu.be/dQw4w9WgXcQ",
            "https://youtube.com.evil.invalid/watch?v=dQw4w9WgXcQ",
            "https://youtube.com@127.0.0.1/watch?v=dQw4w9WgXcQ",
            "https://127.0.0.1/watch?v=dQw4w9WgXcQ",
            "https://www.youtube.com/redirect?q=http://localhost",
            "https://www.youtube.com/watch?v=a&v=dQw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ/more",
            "https://youtu.be:123/dQw4w9WgXcQ",
            "https://www.youtube.com/playlist?list=anything",
            "https://youtu.be/%64Qw4w9WgXcQ",
            "https://youtu.be/dQw4w9WgXcQ\n",
        ] {
            assert!(youtube_url(input).is_err(), "accepted invalid input");
        }
    }

    #[tokio::test]
    async fn stereo_pcm_is_encoded_in_bounded_20ms_frames() {
        let raw: Vec<_> = (0..SAMPLES * 2)
            .flat_map(|i| {
                let sample = ((i / 2) as f32 * 0.05).sin() * 5000.0;
                (sample as i16).to_le_bytes()
            })
            .collect();
        let (tx, mut rx) = mpsc::channel(3);
        encode(raw.as_slice(), tx).await.unwrap();
        let mut decoder = opus::Decoder::new(48_000, opus::Channels::Stereo).unwrap();
        let mut count = 0;
        while let Some(frame) = rx.recv().await {
            let mut decoded = [0_i16; SAMPLES];
            assert_eq!(decoder.decode(&frame, &mut decoded, false).unwrap(), 960);
            assert!(frame.len() <= thiscord_shared::rtp_red::MAX_PAYLOAD);
            assert!(decoded.iter().any(|sample| sample.abs() > 100));
            count += 1;
        }
        assert_eq!(count, 2);
    }

    #[tokio::test]
    async fn partial_tail_is_padded_and_truncated_sample_rejected() {
        let (tx, mut rx) = mpsc::channel(3);
        encode([0_u8; 100].as_slice(), tx).await.unwrap();
        assert!(rx.recv().await.is_some());
        assert!(rx.recv().await.is_none());
        let (tx, _) = mpsc::channel(3);
        assert!(encode([0_u8; 3].as_slice(), tx).await.is_err());
        let (tx, _) = mpsc::channel(3);
        assert!(encode([].as_slice(), tx).await.is_err());
    }

    #[tokio::test]
    async fn bounded_output_backpressures_encoder() {
        let (tx, mut rx) = mpsc::channel(1);
        let raw = vec![0_u8; SAMPLES * 2 * 3];
        let worker = tokio::spawn(async move { encode(raw.as_slice(), tx).await });
        let _ = rx.recv().await.unwrap();
        tokio::task::yield_now().await;
        assert!(!worker.is_finished());
        drop(rx);
        assert!(worker.await.unwrap().is_err());
    }

    #[tokio::test]
    async fn nonexistent_local_media_reports_sanitized_error() {
        let mut player = Player::start(Source::File(PathBuf::from(
            "missing-voice-test-input-49f153",
        )));
        assert!(
            timeout(Duration::from_secs(5), player.frames.recv())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(player.finish().await, Err("Media file unavailable"));
    }

    #[test]
    #[ignore = "private subprocess fixture, launched by the lifecycle test"]
    fn subprocess_fixture() {
        if std::env::var_os("THISCORD_VOICE_PROCESS_FIXTURE").is_none() {
            return;
        }
        use std::io::Write;
        // Synchronize startup so the test cannot pass by killing a process that
        // never ran. No shells, platform-specific sleep executables or ffmpeg.
        std::io::stdout().write_all(b"ready\n").unwrap();
        std::io::stdout().flush().unwrap();
        std::thread::sleep(Duration::from_secs(60));
    }

    #[tokio::test]
    async fn cancellation_kills_and_reaps_both_media_processes() {
        async fn fixture() -> Child {
            let mut process = command(std::env::current_exe().unwrap().to_str().unwrap());
            process
                .args(["subprocess_fixture", "--ignored", "--nocapture"])
                .env("THISCORD_VOICE_PROCESS_FIXTURE", "1");
            let mut child = process.spawn().unwrap();
            let stdout = child.stdout.take().unwrap();
            let mut lines = tokio::io::BufReader::new(stdout);
            use tokio::io::AsyncBufReadExt;
            timeout(Duration::from_secs(10), async {
                loop {
                    let mut line = String::new();
                    assert!(lines.read_line(&mut line).await.unwrap() > 0);
                    if line.contains("ready") {
                        break;
                    }
                }
            })
            .await
            .unwrap();
            assert!(child.try_wait().unwrap().is_none());
            child
        }
        let mut processes = Processes {
            decoder: Some(fixture().await),
            downloader: Some(fixture().await),
        };
        assert!(
            timeout(Duration::from_secs(3), processes.stop())
                .await
                .unwrap()
        );
        assert!(processes.decoder.is_none() && processes.downloader.is_none());
    }

    #[test]
    fn children_do_not_inherit_bot_secrets_or_user_configuration() {
        let child = command("ffmpeg");
        let names: Vec<_> = child
            .as_std()
            .get_envs()
            .map(|(name, _)| name.to_string_lossy().to_ascii_uppercase())
            .collect();
        assert!(names.iter().all(|name| {
            ["PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP", "TMPDIR"].contains(&name.as_str())
        }));
    }
}
