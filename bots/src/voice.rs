//! Outgoing-only native voice, without audio devices or desktop dependencies.
//!
//! Playback is streamed through `ffmpeg` (and `yt-dlp` for YouTube). Install these
//! executables on PATH. No media files are downloaded to disk. `play_*` accepts a
//! request; subsequent decoding/network failures are reported through `status()`.
//! Plain Opus is deliberately negotiated: no RED history, capture, or incoming
//! tracks. Legacy SFU peers do not support strict capture-generation barriers;
//! an unexpected refresh request therefore closes this session, never ignores it.

use super::{Error, Socket};
use futures_util::{SinkExt, StreamExt};
use std::{path::Path, sync::Arc, time::Duration};
use thiscord_shared::{
    ChannelId, GuildId,
    voice::{
        ClientEvent, ClientFrame, Participant, ServerEvent, ServerFrame, VOICE_PATH, VOICE_VERSION,
    },
};
use tokio::{
    sync::{mpsc, oneshot, watch},
    time::{Instant, timeout},
};
use tokio_tungstenite::tungstenite::Message;
use webrtc::media_stream::track_local::TrackLocal;

mod media;
mod peer;
#[cfg(test)]
mod tests;

const IO_TIMEOUT: Duration = Duration::from_secs(2);
const JOIN_TIMEOUT: Duration = Duration::from_secs(25);
const STATE_TIMEOUT: Duration = Duration::from_secs(5);

/// Latest playback state. Error messages contain no paths, URLs or credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlaybackEvent {
    Idle,
    Starting,
    Playing,
    Finished,
    Stopped,
    Error(&'static str),
    Left,
}

/// Clones share one connection. Dropping the final handle cancels playback and
/// closes the session; `leave()` additionally waits for bounded cleanup.
#[derive(Clone)]
pub struct Voice(Arc<Inner>);

struct Inner {
    commands: mpsc::Sender<Command>,
    cancel: watch::Sender<bool>,
    status: watch::Receiver<PlaybackEvent>,
    closed: watch::Receiver<bool>,
}
impl Drop for Inner {
    fn drop(&mut self) {
        self.cancel.send_replace(true);
    }
}
enum Command {
    Play(media::Source, oneshot::Sender<Result<(), Error>>),
    Stop(oneshot::Sender<Result<(), Error>>),
}

impl Voice {
    pub async fn join(
        endpoint: &str,
        origin: &str,
        token: &str,
        guild: GuildId,
        channel: ChannelId,
    ) -> Result<Self, Error> {
        let socket = timeout(
            IO_TIMEOUT * 5,
            super::connect_socket(endpoint, origin, VOICE_PATH),
        )
        .await
        .map_err(|_| Error::message("Voice connection timed out"))??;
        let (commands, rx) = mpsc::channel(8);
        let (cancel, cancellation) = watch::channel(false);
        let (status_tx, status) = watch::channel(PlaybackEvent::Idle);
        let (closed_tx, closed) = watch::channel(false);
        let (ready_tx, ready) = oneshot::channel();
        let voice = Self(Arc::new(Inner {
            commands,
            cancel,
            status,
            closed,
        }));
        let token = token.to_owned();
        tokio::spawn(session(
            socket,
            token,
            guild,
            channel,
            rx,
            cancellation,
            status_tx,
            closed_tx,
            ready_tx,
        ));
        timeout(JOIN_TIMEOUT + IO_TIMEOUT, ready)
            .await
            .map_err(|_| Error::message("Voice join timed out"))?
            .map_err(|_| Error::message("Voice connection closed"))??;
        Ok(voice)
    }

    /// Accept a local file for streaming. Starting another file replaces playback.
    pub async fn play_file(&self, path: impl AsRef<Path>) -> Result<(), Error> {
        self.play(media::Source::File(path.as_ref().to_owned()))
            .await
    }

    /// Only HTTPS YouTube video links are accepted; playlists/live streams and
    /// arbitrary remote media URLs are not supported.
    /// Uses direct HTTPS formats without external JavaScript challenge helpers;
    /// videos requiring those helpers fail through the status watch.
    pub async fn play_youtube(&self, url: &str) -> Result<(), Error> {
        self.play(media::Source::Youtube(media::youtube_url(url)?))
            .await
    }

    async fn play(&self, source: media::Source) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.command(Command::Play(source, tx), rx).await
    }

    /// Stop and reap the current media processes, preserving the voice connection.
    pub async fn stop(&self) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();
        self.command(Command::Stop(tx), rx).await
    }

    async fn command(
        &self,
        command: Command,
        reply: oneshot::Receiver<Result<(), Error>>,
    ) -> Result<(), Error> {
        if *self.0.cancel.borrow() || *self.0.closed.borrow() {
            return Err(Error::message("Voice connection closed"));
        }
        // Reject overload rather than enqueue an operation that might execute
        // long after the caller has timed out.
        self.0
            .commands
            .try_send(command)
            .map_err(|_| Error::message("Voice command queue unavailable"))?;
        reply
            .await
            .map_err(|_| Error::message("Voice connection closed"))?
    }

    pub async fn leave(&self) -> Result<(), Error> {
        self.0.cancel.send_replace(true);
        let mut closed = self.0.closed.clone();
        timeout(Duration::from_secs(10), closed.wait_for(|value| *value))
            .await
            .map_err(|_| Error::message("Voice cleanup timed out"))?
            .map_err(|_| Error::message("Voice cleanup interrupted"))?;
        Ok(())
    }

    pub fn status(&self) -> watch::Receiver<PlaybackEvent> {
        self.0.status.clone()
    }
}

async fn send(socket: &mut Socket, event: ClientEvent) -> Result<(), Error> {
    let text = serde_json::to_string(&ClientFrame {
        version: VOICE_VERSION,
        event,
    })
    .map_err(|_| Error::message("Voice signal encoding failed"))?;
    timeout(IO_TIMEOUT, socket.send(Message::Text(text.into())))
        .await
        .map_err(|_| Error::message("Voice signaling timed out"))?
        .map_err(|_| Error::message("Voice signaling disconnected"))
}

async fn receive(socket: &mut Socket) -> Result<ServerEvent, Error> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) if text.len() <= 128 * 1024 => {
                let frame: ServerFrame = serde_json::from_str(&text)
                    .map_err(|_| Error::message("Invalid voice signal"))?;
                if frame.version != VOICE_VERSION {
                    return Err(Error::message("Unsupported voice protocol"));
                }
                return Ok(frame.event);
            }
            Some(Ok(Message::Ping(_))) => {
                timeout(IO_TIMEOUT, socket.flush())
                    .await
                    .map_err(|_| Error::message("Voice signaling timed out"))?
                    .map_err(|_| Error::message("Voice signaling disconnected"))?;
            }
            Some(Ok(Message::Pong(_))) => {}
            _ => return Err(Error::message("Voice signaling disconnected")),
        }
    }
}

fn own(members: &[Participant], slot: usize) -> Result<&Participant, Error> {
    members
        .iter()
        .find(|member| member.slot == slot)
        .filter(|member| member.can_speak)
        .ok_or_else(|| Error::message("Voice permission revoked"))
}

async fn restricted(socket: &mut Socket, value: bool) -> Result<(), Error> {
    // The SFU disallows *all* publishing while deafened. The SDP is send-only,
    // so clearing deafen for playback still never subscribes to incoming audio.
    send(
        socket,
        ClientEvent::State {
            muted: value,
            deafened: value,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn session(
    mut socket: Socket,
    token: String,
    guild: GuildId,
    channel: ChannelId,
    mut commands: mpsc::Receiver<Command>,
    mut cancel: watch::Receiver<bool>,
    status: watch::Sender<PlaybackEvent>,
    closed: watch::Sender<bool>,
    ready: oneshot::Sender<Result<(), Error>>,
) {
    let mut peer = None;
    let joined = tokio::select! { biased;
        _ = async { let _ = cancel.wait_for(|value| *value).await; } => Err(Error::message("Voice join cancelled")),
        result = timeout(JOIN_TIMEOUT, async {
            send(&mut socket, ClientEvent::Join { token, guild_id: guild, channel_id: channel, attempt: None }).await?;
            let (sdp, servers, slot) = match receive(&mut socket).await? {
                ServerEvent::Offer { sdp, ice_servers, slot, can_speak: true, .. } => (sdp, ice_servers, slot),
                ServerEvent::Error { error } => return Err(Error::api(error)),
                _ => return Err(Error::message("Voice join denied")),
            };
            peer = Some(peer::Peer::new(&servers).await?);
            let p = peer.as_ref().expect("peer created");
            let sdp = p.answer(&sdp).await?;
            send(&mut socket, ClientEvent::Answer {
                sdp, packet_redundancy: false, remote_audio: false,
                screen_feedback: false, screen_subscriptions: false, screen_layers: false,
            }).await?;
            loop {
                match receive(&mut socket).await? {
                    ServerEvent::Participants { members } => { own(&members, slot)?; break; },
                    ServerEvent::Pong {} => {},
                    _ => return Err(Error::message("Voice admission denied")),
                }
            }
            restricted(&mut socket, true).await?;
            // Admission means both the state and the DTLS connection are ready.
            loop {
                match receive(&mut socket).await? {
                    ServerEvent::Participants { members } => {
                        let member = own(&members, slot)?;
                        if member.muted && member.deafened { break; }
                    },
                    ServerEvent::Pong {} => {},
                    _ => return Err(Error::message("Voice admission denied")),
                }
            }
            p.connected().await?;
            Ok(slot)
        }) => result.unwrap_or_else(|_| Err(Error::message("Voice join timed out"))),
    };
    let mut player: Option<media::Player> = None;
    let result = match joined {
        Ok(slot) => {
            let _ = ready.send(Ok(()));
            let mut shutdown = cancel.clone();
            tokio::select! { biased;
                _ = async { let _ = shutdown.wait_for(|value| *value).await; } => Ok(()),
                result = run(
                &mut socket,
                peer.as_ref().expect("joined peer"),
                slot,
                &mut commands,
                &mut cancel,
                &status,
                &mut player,
                ) => result,
            }
        }
        Err(error) => {
            let _ = ready.send(Err(error));
            Err(Error::message("Voice join failed"))
        }
    };
    if let Some(mut player) = player {
        player.stop().await;
    }
    // Close media before waiting on the signaling socket. A disconnected socket
    // must not leave outgoing media alive during the server's resume grace.
    if let Some(mut peer) = peer {
        peer.close().await;
    }
    let _ = timeout(IO_TIMEOUT, async {
        let _ = restricted(&mut socket, true).await;
        let _ = send(&mut socket, ClientEvent::Leave {}).await;
        let _ = socket.close(None).await;
    })
    .await;
    status.send_replace(if result.is_err() && !*cancel.borrow() {
        PlaybackEvent::Error("Voice connection ended")
    } else {
        PlaybackEvent::Left
    });
    closed.send_replace(true);
}

#[allow(clippy::too_many_arguments)]
async fn run(
    socket: &mut Socket,
    peer: &peer::Peer,
    slot: usize,
    commands: &mut mpsc::Receiver<Command>,
    cancel: &mut watch::Receiver<bool>,
    status: &watch::Sender<PlaybackEvent>,
    player: &mut Option<media::Player>,
) -> Result<(), Error> {
    let mut clock = peer::RtpClock::new();
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut pong = Instant::now();
    let mut allowed = false;
    let mut starting = None;
    let mut pending_start: Option<(media::Source, u64)> = None;
    // Join already observed the restricted roster. Track subsequent snapshots,
    // including stop/natural completion, rather than treating each as a reply.
    let mut restriction_known = true;
    let mut pings_sent = 0_u64;
    let mut pongs_received = 0_u64;
    let mut first = true;
    let mut peer_state = peer.state.clone();
    loop {
        // Fair polling prevents a busy command producer from starving permission
        // events or heartbeats. The outer session select prioritizes shutdown.
        tokio::select! {
            _ = async { let _ = cancel.wait_for(|value| *value).await; } => return Ok(()),
            state = peer_state.changed() => {
                if state.is_err() || !peer::usable(*peer_state.borrow_and_update()) {
                    return Err(Error::message("Voice media disconnected"));
                }
            },
            command = commands.recv() => {
                let Some(command) = command else { return Ok(()); };
                allowed = false;
                starting = None;
                pending_start = None;
                if let Some(mut previous) = player.take() { previous.stop().await; }
                restricted(socket, true).await?;
                match command {
                    Command::Stop(reply) => {
                        status.send_replace(PlaybackEvent::Stopped);
                        let _ = reply.send(Ok(()));
                    },
                    Command::Play(source, reply) => {
                        // Closed callers cannot accidentally start queued playback.
                        if reply.is_closed() { continue; }
                        // A Participants snapshot has no request ID and may be
                        // stale. The server handles socket signals serially, so
                        // this Pong fences every snapshot queued before mute.
                        // Count all Pings: an earlier heartbeat Pong is not proof.
                        send(socket, ClientEvent::Ping {}).await?;
                        pings_sent += 1;
                        pending_start = Some((source, pings_sent));
                        starting = Some(Instant::now());
                        first = true;
                        status.send_replace(PlaybackEvent::Starting);
                        let _ = reply.send(Ok(()));
                    },
                }
            },
            event = receive(socket) => match event? {
                ServerEvent::Pong {} => {
                    if pongs_received == pings_sent {
                        return Err(Error::message("Unexpected voice heartbeat"));
                    }
                    pongs_received += 1;
                    pong = Instant::now();
                },
                ServerEvent::Participants { members } => {
                    let member = own(&members, slot)?;
                    restriction_known = member.muted && member.deafened;
                    if player.is_some() {
                        if !member.muted && !member.deafened {
                            allowed = true;
                            starting = None;
                        } else if allowed {
                            // A state restriction never auto-resumes old PCM.
                            allowed = false;
                            if let Some(mut previous) = player.take() { previous.stop().await; }
                            restricted(socket, true).await?;
                            status.send_replace(PlaybackEvent::Stopped);
                        }
                    }
                },
                ServerEvent::MicrophoneRefreshRequired { .. } |
                ServerEvent::MicrophoneBoundaryAccepted { .. } => {
                    // SFU red::request explicitly excludes legacy publishers.
                    // Fail closed if that contract ever changes.
                    return Err(Error::message("Voice privacy renegotiation required"));
                },
                ServerEvent::Revoked {} | ServerEvent::Replaced {} | ServerEvent::Error { .. } |
                ServerEvent::Offer { .. } | ServerEvent::ResumeUnavailable {} => {
                    return Err(Error::message("Voice access ended"));
                },
                _ => {},
            },
            _ = heartbeat.tick() => {
                if pong.elapsed() > Duration::from_secs(45) { return Err(Error::message("Voice heartbeat timed out")); }
                send(socket, ClientEvent::Ping {}).await?;
                pings_sent += 1;
            },
            _ = tick.tick() => {
                if starting.is_some_and(|at: Instant| at.elapsed() >= STATE_TIMEOUT) {
                    return Err(Error::message("Voice state acknowledgement timed out"));
                }
                if !allowed { continue; }
                let Some(current) = player.as_mut() else { continue; };
                match current.frames.try_recv() {
                    Ok(payload) => {
                        let packet = clock.packet(payload, first);
                        timeout(Duration::from_millis(250), peer.track.write_rtp(packet)).await
                            .map_err(|_| Error::message("Voice media write timed out"))?
                            .map_err(|_| Error::message("Voice media write failed"))?;
                        if first { status.send_replace(PlaybackEvent::Playing); first = false; }
                    },
                    Err(mpsc::error::TryRecvError::Empty) => {},
                    Err(mpsc::error::TryRecvError::Disconnected) => {
                        allowed = false;
                        let mut finished = player.take().expect("active player");
                        let result = finished.finish().await;
                        restricted(socket, true).await?;
                        status.send_replace(match result {
                            Ok(()) => PlaybackEvent::Finished,
                            Err(message) => PlaybackEvent::Error(message),
                        });
                    },
                }
            },
        }
        if restriction_known
            && pending_start
                .as_ref()
                .is_some_and(|(_, fence)| pongs_received >= *fence)
        {
            let (source, _) = pending_start.take().expect("pending start");
            // Do not send mute+unmute together: room.changed may coalesce them.
            // Only a restricted roster plus the ordered Pong permits this step.
            restricted(socket, false).await?;
            restriction_known = false;
            *player = Some(media::Player::start(source));
        }
    }
}
