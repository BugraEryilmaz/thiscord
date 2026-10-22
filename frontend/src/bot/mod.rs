//! Headless client companion, built in the frontend package to reuse native audio.
mod control;
mod peer;
mod video;
use crate::audio;
use rtc::{rtp, rtp_transceiver::rtp_sender::*};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex as StdMutex},
    time::{Duration, Instant},
};
use thiscord_shared::{AccountId, ChannelId, GuildId, voice::*, voice_bots::*};
use tokio::sync::{Notify, mpsc, watch};
use webrtc::{
    media_stream::{
        track_local::TrackLocal,
        track_remote::{TrackRemote, TrackRemoteEvent},
    },
    peer_connection::*,
};

#[derive(Debug)]
enum Failure {
    Unavailable,
    Forbidden,
    Limited,
}
impl From<String> for Failure {
    fn from(_: String) -> Self {
        Self::Unavailable
    }
}
impl From<&str> for Failure {
    fn from(_: &str) -> Self {
        Self::Unavailable
    }
}

pub async fn run_stdio() -> Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut input = tokio::io::stdin();
    let mut output = tokio::io::stdout();
    async fn read(input: &mut (impl AsyncReadExt + Unpin)) -> Result<HostMessage, String> {
        let size = input.read_u32().await.map_err(|_| "Parent disconnected")? as usize;
        if size > IPC_LIMIT {
            return Err("Invalid parent frame".into());
        }
        let mut bytes = vec![0; size];
        input
            .read_exact(&mut bytes)
            .await
            .map_err(|_| "Parent disconnected")?;
        serde_json::from_slice(&bytes).map_err(|_| "Invalid parent message".into())
    }
    let HostMessage::Start {
        token,
        guild,
        channel,
        bot,
    } = read(&mut input).await?
    else {
        return Err("Missing bootstrap".into());
    };
    bot.settings.validate()?;
    let _logs = control::Logs::new(bot.id)?;
    audio::virtual_devices::initialize(&bot.settings);
    let (settings, settings_rx) = watch::channel(bot.settings.clone());
    let bot = Arc::new(StdMutex::new(bot));
    let (stop, stop_rx) = watch::channel(false);
    let (messages, mut inputs) = mpsc::channel(32);
    let reader = tokio::spawn(async move {
        while let Ok(message) = read(&mut input).await {
            if messages.send(message).await.is_err() {
                break;
            }
        }
    });
    struct Reader(tokio::task::JoinHandle<()>);
    impl Drop for Reader {
        fn drop(&mut self) {
            self.0.abort();
        }
    }
    let _reader = Reader(reader);
    let (tx, mut outgoing) = mpsc::channel(32);
    let (incoming, rx) = mpsc::channel(32);
    let (ready, mut readiness) = tokio::sync::oneshot::channel();
    let peer = peer::run(
        tx,
        rx,
        token,
        guild,
        channel,
        settings_rx,
        stop_rx,
        bot.clone(),
        ready,
    );
    tokio::pin!(peer);
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut waiting = true;
    let mut finished = false;
    loop {
        let message = tokio::select! { biased;
            _ = &mut peer => {
                finished = true;
                if waiting { Some(PeerMessage::Ready(readiness.try_recv().unwrap_or(Err(Failure::Unavailable)).map_err(startup_failure))) } else { None }
            },
            result = &mut readiness, if waiting => { waiting = false; Some(PeerMessage::Ready(result.unwrap_or(Err(Failure::Unavailable)).map_err(startup_failure))) },
            message = inputs.recv() => match message {
                Some(HostMessage::Signal(text)) => { incoming.try_send(text).map_err(|_| "Signaling queue full")?; None },
                Some(HostMessage::Settings(mut next)) => {
                    next.validate()?;
                    // Audio edits use the same serialized remote-control protocol as desktops.
                    next.audio = bot.lock().unwrap().settings.audio.clone();
                    settings.send_replace(next); None
                },
                _ => { stop.send_replace(true); break; }
            },
            signal = outgoing.recv() => signal.map(PeerMessage::Signal),
            _ = tick.tick(), if !waiting => Some(PeerMessage::Snapshot(Box::new(bot.lock().unwrap().clone()))),
        };
        if let Some(message) = message {
            let bytes = serde_json::to_vec(&message).map_err(|_| "Invalid companion message")?;
            if bytes.len() > IPC_LIMIT {
                return Err("Companion frame too large".into());
            }
            tokio::time::timeout(Duration::from_secs(2), async {
                output.write_u32(bytes.len() as u32).await?;
                output.write_all(&bytes).await?;
                output.flush().await
            })
            .await
            .map_err(|_| "Parent blocked")?
            .map_err(|_| "Parent disconnected")?;
        }
        if finished {
            break;
        }
    }
    Ok(())
}

fn configuration(servers: &[IceServer]) -> RTCConfiguration {
    RTCConfigurationBuilder::new()
        .with_ice_servers(
            servers
                .iter()
                .map(|s| RTCIceServer {
                    urls: s.urls.clone(),
                    username: s.username.clone(),
                    credential: s.credential.clone(),
                })
                .collect(),
        )
        .build()
}

fn startup_failure(error: Failure) -> StartupFailure {
    match error {
        Failure::Unavailable => StartupFailure::Unavailable,
        Failure::Forbidden => StartupFailure::Forbidden,
        Failure::Limited => StartupFailure::Limited,
    }
}
