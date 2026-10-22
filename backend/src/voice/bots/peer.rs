//! Private stdio signaling bridge to the frontend's headless native client.
use super::*;
use std::process::Stdio;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[allow(clippy::too_many_arguments)]
pub(super) async fn run(
    tx: mpsc::Sender<Message>,
    mut rx: mpsc::Receiver<Message>,
    token: String,
    guild: GuildId,
    channel: ChannelId,
    mut settings: watch::Receiver<Settings>,
    mut stop: watch::Receiver<bool>,
    bot: Arc<StdMutex<Bot>>,
    ready: tokio::sync::oneshot::Sender<Result<(), Failure>>,
) {
    let mut ready = Some(ready);
    let work = async {
        let path = std::env::var_os("THISCORD_VOICE_BOT_PATH")
            .map(std::path::PathBuf::from)
            .or_else(|| {
                std::env::current_exe().ok().map(|p| {
                    p.with_file_name(if cfg!(windows) {
                        "thiscord-voice-bot.exe"
                    } else {
                        "thiscord-voice-bot"
                    })
                })
            })
            .ok_or(Failure::Unavailable)?;
        let mut process = tokio::process::Command::new(path);
        process
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        #[cfg(windows)]
        process.creation_flags(0x08000000); // CREATE_NO_WINDOW
        let mut child = process.spawn().map_err(|_| Failure::Unavailable)?;
        let mut input = child.stdin.take().ok_or(Failure::Unavailable)?;
        let mut output = child.stdout.take().ok_or(Failure::Unavailable)?;
        let (writer, mut writes) = mpsc::channel::<HostMessage>(32);
        let (reads, mut reader) = mpsc::channel::<PeerMessage>(32);
        let write_task = tokio::spawn(async move {
            while let Some(message) = writes.recv().await {
                let bytes = serde_json::to_vec(&message).map_err(|_| ())?;
                if bytes.len() > IPC_LIMIT {
                    return Err(());
                }
                input.write_u32(bytes.len() as u32).await.map_err(|_| ())?;
                input.write_all(&bytes).await.map_err(|_| ())?;
                input.flush().await.map_err(|_| ())?;
            }
            Ok::<_, ()>(())
        });
        let read_task = tokio::spawn(async move {
            loop {
                let size = output.read_u32().await.map_err(|_| ())? as usize;
                if size > IPC_LIMIT {
                    return Err(());
                }
                let mut bytes = vec![0; size];
                output.read_exact(&mut bytes).await.map_err(|_| ())?;
                let message = serde_json::from_slice(&bytes).map_err(|_| ())?;
                reads.send(message).await.map_err(|_| ())?;
            }
        });
        struct Tasks(
            tokio::task::JoinHandle<Result<(), ()>>,
            tokio::task::JoinHandle<Result<(), ()>>,
        );
        impl Drop for Tasks {
            fn drop(&mut self) {
                self.0.abort();
                self.1.abort();
            }
        }
        let _tasks = Tasks(write_task, read_task);
        writer
            .try_send(HostMessage::Start {
                token,
                guild,
                channel,
                bot: bot.lock().unwrap().clone(),
            })
            .map_err(|_| Failure::Unavailable)?;
        let mut server_closed = false;
        loop {
            tokio::select! { biased;
                _ = stop.wait_for(|s| *s) => break,
                message = reader.recv() => match message {
                    Some(PeerMessage::Signal(text)) => tx.try_send(Message::Text(text.into())).map_err(|_| Failure::Unavailable)?,
                    Some(PeerMessage::Ready(result)) => { if let Some(ready) = ready.take() { let _ = ready.send(result.map_err(|error| match error { StartupFailure::Unavailable => Failure::Unavailable, StartupFailure::Forbidden => Failure::Forbidden, StartupFailure::Limited => Failure::Limited })); } },
                    Some(PeerMessage::Snapshot(snapshot)) => {
                        let mut bot = bot.lock().unwrap();
                        bot.settings.audio = snapshot.settings.audio;
                        bot.sent_packets = snapshot.sent_packets; bot.dropped_packets = snapshot.dropped_packets;
                        bot.received_packets = snapshot.received_packets; bot.output_peak = snapshot.output_peak;
                        bot.video = snapshot.video; bot.received_system_audio_packets = snapshot.received_system_audio_packets;
                    },
                    None => break,
                },
                signal = rx.recv(), if !server_closed => match signal {
                    Some(Message::Text(text)) => writer.try_send(HostMessage::Signal(text.to_string())).map_err(|_| Failure::Unavailable)?,
                    _ if ready.is_some() => server_closed = true,
                    _ => break,
                },
                result = settings.changed() => {
                    if result.is_err() { break; }
                    writer.try_send(HostMessage::Settings(settings.borrow_and_update().clone())).map_err(|_| Failure::Unavailable)?;
                },
                _ = tokio::time::sleep(Duration::from_secs(2)), if server_closed => break,
            }
        }
        // EOF asks the companion to stop normally; kill remains the bounded fallback.
        drop(writer);
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        Ok(())
    };
    let result = tokio::select! {
        result = work => result,
        _ = tokio::time::sleep(Duration::from_secs(LIFETIME_SECS)) => Ok(()),
    };
    if let Some(ready) = ready {
        let _ = ready.send(result.and(Err(Failure::Unavailable)));
    }
}
