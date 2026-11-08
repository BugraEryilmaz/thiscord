//! Sharded sans-I/O SFU transport, selected with `THISCORD_VOICE_TRANSPORT=shard`
//! (default `webrtc`, the per-peer webrtc-rs driver). See docs/sfu-transport.md.
//!
//! Each shard is one thread running one event loop over one UDP socket. It owns
//! the `rtc` sans-I/O cores of every peer in the rooms hashed to it, so a room's
//! publishers and receivers live on the same loop: a received packet is
//! decrypted, authorized, rewritten and encrypted for every receiver without a
//! task, channel or lock hand-off in between. Receives and sends are batched
//! (`recvmmsg`/`sendmmsg` on Linux) and one timer serves the whole shard.
//!
//! Signaling, authorization rechecks and room state stay on the media runtime
//! exactly as in the webrtc mode; only the transport and the per-packet relay
//! and egress steps (`forward.rs`) run here. Shards never block on the room lock
//! for long: writers hold it only for synchronous updates.
mod demux;
mod forward;
mod peer;
mod udp;

use super::*;
use bytes::BytesMut;
use peer::Connection;
use rtc::{
    peer_connection::{RTCPeerConnection, sdp::RTCSessionDescription},
    sansio::Protocol,
    shared::{TaggedBytesMut, TransportContext, TransportProtocol},
    statistics::{StatsSelector, report::RTCStatsReport},
};
use std::{cmp::Reverse, collections::BinaryHeap, net::SocketAddr};
use tokio::sync::oneshot;

pub const TRANSPORT_VAR: &str = "THISCORD_VOICE_TRANSPORT";
pub const SHARDS_VAR: &str = "THISCORD_VOICE_SHARDS";
const SHARD_RANGE: std::ops::RangeInclusive<usize> = 1..=64;
/// Default shard count when unset: one per core, at most this many.
const DEFAULT_MAX_SHARDS: usize = 16;
/// Receive rounds per wake before the loop serves commands and timers again.
const MAX_RECEIVE_ROUNDS: usize = 16;
/// Commands handled per wake, so signaling bursts cannot starve media.
const MAX_COMMANDS_PER_WAKE: usize = 64;
/// See webrtc-rs's driver (webrtc#862): an expired deadline is not handled
/// again for the same peer within this interval, so a timer stuck in the past
/// cannot spin the shard.
const MIN_IMMEDIATE_TIMEOUT: Duration = Duration::from_millis(1);
/// How long a shard waits for socket capacity before dropping what it could
/// not send. UDP gives no delivery guarantee; a stalled send must not stall
/// every room on the shard.
const SEND_WAIT: Duration = Duration::from_millis(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Transport {
    /// One webrtc-rs `PeerConnection` (driver task, socket) per participant.
    WebRtc,
    /// Sharded sans-I/O loops with in-loop fan-out.
    Shard { shards: usize },
}

impl Transport {
    /// Parses the optional variables; any other value fails startup.
    pub fn parse(transport: Option<&str>, shards: Option<&str>) -> Result<Self, crate::BoxError> {
        let shards = match shards {
            None => None,
            Some(value) => Some(
                value
                    .trim()
                    .parse::<usize>()
                    .ok()
                    .filter(|n| SHARD_RANGE.contains(n))
                    .ok_or_else(|| {
                        format!(
                            "{SHARDS_VAR} must be an integer from {} to {}",
                            SHARD_RANGE.start(),
                            SHARD_RANGE.end()
                        )
                    })?,
            ),
        };
        match transport.map(str::trim) {
            None | Some("webrtc") => {
                if shards.is_some() {
                    return Err(format!("{SHARDS_VAR} requires {TRANSPORT_VAR}=shard").into());
                }
                Ok(Self::WebRtc)
            }
            Some("shard") => Ok(Self::Shard {
                shards: shards.unwrap_or_else(|| {
                    std::thread::available_parallelism()
                        .map_or(1, usize::from)
                        .min(DEFAULT_MAX_SHARDS)
                }),
            }),
            Some(_) => Err(format!("{TRANSPORT_VAR} must be \"webrtc\" or \"shard\"").into()),
        }
    }

    pub fn from_env() -> Result<Self, crate::BoxError> {
        let read = |name: &str| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(_) => Err(format!("{name} must contain valid Unicode")),
        };
        Self::parse(
            read(TRANSPORT_VAR)?.as_deref(),
            read(SHARDS_VAR)?.as_deref(),
        )
    }
}

static TRANSPORT: OnceLock<Transport> = OnceLock::new();

/// Fixes the process's transport (startup validation). Later calls with a
/// different value fail; the first voice connection fixes it otherwise.
pub fn configure(transport: Transport) -> Result<(), crate::BoxError> {
    let current = *TRANSPORT.get_or_init(|| transport);
    if current != transport {
        return Err("voice transport already configured".into());
    }
    Ok(())
}

/// The configured transport. Invalid variables were rejected at startup;
/// tests and examples that skip startup validation fail here instead of
/// silently running a different transport than requested.
pub(super) fn transport() -> Transport {
    *TRANSPORT.get_or_init(|| Transport::from_env().expect("invalid voice transport configuration"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct PeerId {
    index: u32,
    generation: u32,
}

/// What the shard needs from the signaling task to relay for one connection.
pub(super) struct PeerContext {
    pub request_id: RequestId,
    pub room: Arc<Room>,
    pub slot: usize,
    pub source_id: uuid::Uuid,
    pub access: Arc<access::Access>,
    pub generation: Arc<AtomicU64>,
    pub active: Arc<AtomicBool>,
    pub metrics: Arc<[AtomicU64; 5]>,
}

enum Command {
    Create {
        context: Box<PeerContext>,
        reply: oneshot::Sender<Result<PeerId, ()>>,
    },
    Offer {
        peer: PeerId,
        reply: oneshot::Sender<Result<RTCSessionDescription, ()>>,
    },
    /// An ICE restart offer for a negotiated connection (new local ufrag).
    RestartIce {
        peer: PeerId,
        reply: oneshot::Sender<Result<RTCSessionDescription, ()>>,
    },
    Answer {
        peer: PeerId,
        answer: Box<RTCSessionDescription>,
        browser: bool,
        reply: oneshot::Sender<Result<(), ()>>,
    },
    /// Negotiated: relay this peer's media and deliver the room's media to it.
    Activate {
        peer: PeerId,
    },
    /// RTCP PLI to a browser publisher's video SSRC.
    Keyframe {
        peer: PeerId,
        kind: MediaKind,
    },
    Stats {
        peer: PeerId,
        reply: oneshot::Sender<Option<RTCStatsReport>>,
    },
    Close {
        peer: PeerId,
    },
}

struct Handle {
    commands: mpsc::UnboundedSender<Command>,
}

static SHARDS: OnceLock<Vec<Handle>> = OnceLock::new();

fn shards(count: usize) -> &'static [Handle] {
    SHARDS.get_or_init(|| {
        let base = local_bind()
            .ok()
            .and_then(|address| address.parse::<SocketAddr>().ok());
        (0..count)
            .map(|index| {
                let (commands, receiver) = mpsc::unbounded_channel();
                match base
                    .ok_or_else(|| {
                        std::io::Error::other(
                            "THISCORD_VOICE_BIND must be an IP:port socket address",
                        )
                    })
                    .and_then(|base| spawn(index, base, receiver))
                {
                    Ok(local) => {
                        tracing::info!(shard = index, local_addr = %local, "voice shard started");
                    }
                    Err(error) => {
                        // The receiver is gone with the failed thread; every join
                        // routed here fails with Unavailable instead of hanging.
                        tracing::error!(shard = index, %error, "voice shard failed to start");
                    }
                }
                Handle { commands }
            })
            .collect()
    })
}

fn spawn(
    index: usize,
    base: SocketAddr,
    commands: mpsc::UnboundedReceiver<Command>,
) -> std::io::Result<SocketAddr> {
    if base.ip().is_unspecified() {
        // Host candidates name the bound address; a wildcard is not one.
        return Err(std::io::Error::other(
            "the shard transport needs a concrete media address, not a wildcard",
        ));
    }
    // A fixed configured port gives shard i the port + i.
    let port = if base.port() == 0 {
        0
    } else {
        base.port()
            .checked_add(u16::try_from(index).map_err(std::io::Error::other)?)
            .ok_or_else(|| std::io::Error::other("voice shard port out of range"))?
    };
    let socket = udp::bind(SocketAddr::new(base.ip(), port))?;
    let local = socket.local_addr()?;
    let media = media_runtime().clone();
    std::thread::Builder::new()
        .name(format!("thiscord-sfu-{index}"))
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    tracing::error!(shard = index, %error, "voice shard runtime failed");
                    return;
                }
            };
            runtime.block_on(async move {
                match udp::Socket::new(socket) {
                    Ok(socket) => Shard::new(socket, media).run(commands).await,
                    Err(error) => {
                        tracing::error!(shard = index, %error, "voice shard socket failed");
                    }
                }
            });
        })?;
    Ok(local)
}

fn shard_for(channel: ChannelId, count: usize) -> &'static Handle {
    let id = channel.as_uuid().as_u128();
    let hash = (id ^ (id >> 64)) as u64;
    &shards(count)[(hash % count as u64) as usize]
}

/// A connection on its shard. Dropping it closes the connection there.
pub(super) struct Peer {
    link: Link,
}

/// A non-owning reference for tasks that only ask questions (stats sampling).
#[derive(Clone)]
pub(super) struct Link {
    id: PeerId,
    commands: mpsc::UnboundedSender<Command>,
}

impl Peer {
    pub async fn create(channel: ChannelId, context: PeerContext) -> Result<Self, ()> {
        let Transport::Shard { shards } = transport() else {
            return Err(());
        };
        let commands = shard_for(channel, shards).commands.clone();
        let (reply, response) = oneshot::channel();
        commands
            .send(Command::Create {
                context: Box::new(context),
                reply,
            })
            .map_err(|_| ())?;
        let id = response.await.map_err(|_| ())??;
        Ok(Self {
            link: Link { id, commands },
        })
    }

    pub fn link(&self) -> Link {
        self.link.clone()
    }

    /// Adds the relay tracks, creates and applies the offer and gathers the
    /// shard's host candidate. Returns the complete local description.
    pub async fn offer(&self) -> Result<RTCSessionDescription, ()> {
        let (reply, response) = oneshot::channel();
        self.link.send(Command::Offer {
            peer: self.link.id,
            reply,
        })?;
        response.await.map_err(|_| ())?
    }

    /// Restarts ICE: a new local ufrag over the same shard socket and host
    /// candidate. Applied with `answer`, like the first offer.
    pub async fn restart_offer(&self) -> Result<RTCSessionDescription, ()> {
        let (reply, response) = oneshot::channel();
        self.link.send(Command::RestartIce {
            peer: self.link.id,
            reply,
        })?;
        response.await.map_err(|_| ())?
    }

    pub async fn answer(&self, answer: RTCSessionDescription, browser: bool) -> Result<(), ()> {
        let (reply, response) = oneshot::channel();
        self.link.send(Command::Answer {
            peer: self.link.id,
            answer: Box::new(answer),
            browser,
            reply,
        })?;
        response.await.map_err(|_| ())?
    }

    pub fn activate(&self) {
        let _ = self.link.send(Command::Activate { peer: self.link.id });
    }

    pub fn keyframe(&self, kind: MediaKind) {
        let _ = self.link.send(Command::Keyframe {
            peer: self.link.id,
            kind,
        });
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.link.send(Command::Close { peer: self.link.id });
    }
}

impl Link {
    fn send(&self, command: Command) -> Result<(), ()> {
        self.commands.send(command).map_err(|_| ())
    }

    pub async fn stats(&self) -> Option<RTCStatsReport> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Stats {
            peer: self.id,
            reply,
        })
        .ok()?;
        response.await.ok().flatten()
    }
}

/// The selected candidate pair's local and remote candidate types, from stats.
pub(super) fn selected_route(report: &RTCStatsReport) -> Option<(String, String)> {
    use rtc::statistics::report::RTCStatsReportEntry as Entry;
    let pair = report.iter().find_map(|entry| match entry {
        Entry::IceCandidatePair(pair) if pair.nominated => Some(pair),
        _ => None,
    })?;
    let candidate = |id: &str, local: bool| {
        report.iter().find_map(|entry| match entry {
            Entry::LocalCandidate(c) if local && c.stats.id == id => {
                Some(c.candidate_type.to_string())
            }
            Entry::RemoteCandidate(c) if !local && c.stats.id == id => {
                Some(c.candidate_type.to_string())
            }
            _ => None,
        })
    };
    Some((
        candidate(&pair.local_candidate_id, true)?,
        candidate(&pair.remote_candidate_id, false)?,
    ))
}

struct Slot {
    generation: u32,
    connection: Option<Box<Connection>>,
}

/// A keyframe request owed after a video drop, sent once the room lock is released.
#[derive(Clone)]
struct Recovery {
    room: Arc<Room>,
    viewer: usize,
    publisher: usize,
    kind: MediaKind,
    epoch: u32,
    active: Arc<AtomicBool>,
}

struct Shard {
    socket: udp::Socket,
    media: tokio::runtime::Handle,
    slots: Vec<Slot>,
    free: Vec<u32>,
    demux: demux::Demux,
    /// Room (by address; members hold it alive) to its activated connections.
    rooms: HashMap<usize, [Option<PeerId>; ROOM_CAPACITY]>,
    timers: BinaryHeap<Reverse<(Instant, PeerId)>>,
    /// Peers with new input this round: events and media reads to drain.
    inputs: Vec<PeerId>,
    /// Peers with possible output this round (a superset of `inputs`).
    outputs: Vec<PeerId>,
    received: Vec<(SocketAddr, BytesMut)>,
    outbound: Vec<(SocketAddr, BytesMut)>,
    media_reads: Vec<(MediaKind, rtp::Packet, Instant)>,
    recovery: Vec<Recovery>,
    /// Last recovery spawned per (room, viewer, publisher, kind). The webrtc
    /// relay and writers await each request; here they would pile up behind
    /// the room's write lock, so each pair spawns at most one per interval.
    recovered: HashMap<(usize, usize, usize, MediaKind), Instant>,
    now: Instant,
}

const RECOVERY_INTERVAL: Duration = Duration::from_millis(250);

/// A connection by id, borrowing only the slot table.
fn connection_mut(slots: &mut [Slot], id: PeerId) -> Option<&mut Connection> {
    slots
        .get_mut(id.index as usize)
        .filter(|slot| slot.generation == id.generation)
        .and_then(|slot| slot.connection.as_deref_mut())
}

enum Wake {
    Command(Option<Command>),
    Readable(std::io::Result<()>),
    Timer,
}

impl Shard {
    fn new(socket: udp::Socket, media: tokio::runtime::Handle) -> Self {
        Self {
            socket,
            media,
            slots: Vec::new(),
            free: Vec::new(),
            demux: Default::default(),
            rooms: HashMap::new(),
            timers: BinaryHeap::new(),
            inputs: Vec::new(),
            outputs: Vec::new(),
            received: Vec::new(),
            outbound: Vec::new(),
            media_reads: Vec::new(),
            recovery: Vec::new(),
            recovered: HashMap::new(),
            now: Instant::now(),
        }
    }

    async fn run(mut self, mut commands: mpsc::UnboundedReceiver<Command>) {
        // A far deadline instead of an Option keeps one reusable timer entry.
        let far = || Instant::now() + Duration::from_secs(86_400);
        let sleep = tokio::time::sleep_until(far().into());
        tokio::pin!(sleep);
        let mut armed: Option<Instant> = None;
        loop {
            let next = self.next_deadline();
            if next != armed {
                sleep
                    .as_mut()
                    .reset(next.unwrap_or_else(far).max(Instant::now()).into());
                armed = next;
            }
            let wake = tokio::select! { biased;
                command = commands.recv() => Wake::Command(command),
                ready = self.socket.readable() => Wake::Readable(ready),
                _ = &mut sleep => Wake::Timer,
            };
            self.now = Instant::now();
            match wake {
                Wake::Command(None) => break,
                Wake::Command(Some(command)) => {
                    self.command(command);
                    for _ in 1..MAX_COMMANDS_PER_WAKE {
                        match commands.try_recv() {
                            Ok(command) => self.command(command),
                            Err(_) => break,
                        }
                    }
                }
                Wake::Readable(Ok(())) => self.receive(),
                Wake::Readable(Err(error)) => {
                    tracing::error!(%error, "voice shard socket failed");
                    break;
                }
                Wake::Timer => armed = None,
            }
            self.timeouts();
            self.process().await;
            self.flush().await;
        }
        for index in 0..self.slots.len() {
            if let Some(connection) = self.slots[index].connection.as_ref() {
                let id = connection.id;
                self.close(id);
            }
        }
    }

    fn next_deadline(&mut self) -> Option<Instant> {
        // Discard stale heap entries so the timer is armed for a live deadline.
        while let Some(Reverse((at, id))) = self.timers.peek().copied() {
            if self.get(id).is_some_and(|c| c.deadline == Some(at)) {
                return Some(at);
            }
            self.timers.pop();
        }
        None
    }

    fn get(&self, id: PeerId) -> Option<&Connection> {
        self.slots
            .get(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
            .and_then(|slot| slot.connection.as_deref())
    }

    fn get_mut(&mut self, id: PeerId) -> Option<&mut Connection> {
        connection_mut(&mut self.slots, id)
    }

    /// Marks a peer for event/read draining and output.
    fn input(&mut self, id: PeerId) {
        if let Some(connection) = self.get_mut(id)
            && !connection.input
        {
            connection.input = true;
            self.inputs.push(id);
        }
        self.output(id);
    }

    /// Marks a peer for output draining only.
    fn output(&mut self, id: PeerId) {
        if let Some(connection) = self.get_mut(id)
            && !connection.output
        {
            connection.output = true;
            self.outputs.push(id);
        }
    }

    fn command(&mut self, command: Command) {
        let now = self.now;
        match command {
            Command::Create { context, reply } => {
                let index = self.free.pop().unwrap_or_else(|| {
                    self.slots.push(Slot {
                        generation: 0,
                        connection: None,
                    });
                    (self.slots.len() - 1) as u32
                });
                let slot = &mut self.slots[index as usize];
                slot.generation = slot.generation.wrapping_add(1);
                let id = PeerId {
                    index,
                    generation: slot.generation,
                };
                match Connection::new(id, *context, now) {
                    Ok(connection) => {
                        slot.connection = Some(Box::new(connection));
                        if reply.send(Ok(id)).is_err() {
                            self.close(id);
                        }
                    }
                    Err(()) => {
                        self.free.push(index);
                        let _ = reply.send(Err(()));
                    }
                }
            }
            Command::Offer { peer, reply } => {
                let local = self.socket.local_addr();
                let result = match self.get_mut(peer) {
                    Some(connection) => connection.offer(now, local),
                    None => Err(()),
                };
                if let Ok(description) = &result
                    && let Some(ufrag) = demux::sdp_ufrag(&description.sdp)
                {
                    self.demux.register(peer, ufrag);
                }
                self.input(peer);
                let _ = reply.send(result);
            }
            Command::RestartIce { peer, reply } => {
                let local = self.socket.local_addr();
                let result = match self.get_mut(peer) {
                    Some(connection) => connection.restart_offer(now, local),
                    None => Err(()),
                };
                // The new ufrag routes from now on. Learned addresses stay, so
                // the current path keeps working until the restart completes.
                if let Ok(description) = &result
                    && let Some(ufrag) = demux::sdp_ufrag(&description.sdp)
                {
                    self.demux.register(peer, ufrag);
                }
                self.input(peer);
                self.output(peer);
                let _ = reply.send(result);
            }
            Command::Answer {
                peer,
                answer,
                browser,
                reply,
            } => {
                let result = match self.get_mut(peer) {
                    Some(connection) => {
                        // Tracks open only after the remote description; classify first.
                        connection.browser = browser;
                        connection
                            .core
                            .set_remote_description(now, *answer)
                            .map_err(|_| ())
                    }
                    None => Err(()),
                };
                self.input(peer);
                let _ = reply.send(result);
            }
            Command::Activate { peer } => {
                let Some(connection) = self.get_mut(peer) else {
                    return;
                };
                connection.forwarding = true;
                let key = Arc::as_ptr(&connection.context.room) as usize;
                let slot = connection.context.slot;
                self.rooms.entry(key).or_default()[slot] = Some(peer);
            }
            Command::Keyframe { peer, kind } => {
                if let Some(connection) = self.get_mut(peer) {
                    connection.keyframe(now, kind);
                }
                self.output(peer);
            }
            Command::Stats { peer, reply } => {
                let report = self
                    .get_mut(peer)
                    .map(|connection| connection.core.get_stats(now, StatsSelector::None));
                let _ = reply.send(report);
            }
            Command::Close { peer } => self.close(peer),
        }
    }

    fn close(&mut self, id: PeerId) {
        let Some(slot) = self
            .slots
            .get_mut(id.index as usize)
            .filter(|slot| slot.generation == id.generation)
        else {
            return;
        };
        let Some(mut connection) = slot.connection.take() else {
            return;
        };
        self.free.push(id.index);
        connection.context.active.store(false, Ordering::Release);
        // Best effort DTLS close_notify; nothing is flushed for this peer later.
        let _ = connection.core.close();
        while let Some(message) = connection.core.poll_write() {
            self.outbound
                .push((message.transport.peer_addr, message.message));
        }
        self.demux.forget(id);
        let key = Arc::as_ptr(&connection.context.room) as usize;
        if let Some(room) = self.rooms.get_mut(&key) {
            let slot = &mut room[connection.context.slot];
            if *slot == Some(id) {
                *slot = None;
            }
            if room.iter().all(Option::is_none) {
                self.rooms.remove(&key);
            }
        }
    }

    fn receive(&mut self) {
        for _ in 0..MAX_RECEIVE_ROUNDS {
            let mut received = std::mem::take(&mut self.received);
            let count = match self
                .socket
                .try_receive(|from, data| received.push((from, BytesMut::from(data))))
            {
                Ok(count) => count,
                Err(error) => {
                    // ICMP-induced errors (ECONNREFUSED) report one bad peer,
                    // not a broken socket; keep serving the others.
                    tracing::debug!(%error, "voice shard receive error");
                    0
                }
            };
            for (from, data) in received.drain(..) {
                self.deliver(from, data);
            }
            self.received = received;
            if count < udp::RECEIVE_BATCH {
                break;
            }
        }
    }

    fn deliver(&mut self, from: SocketAddr, data: BytesMut) {
        let Some(id) = self.demux.route(from, &data) else {
            return;
        };
        let local_addr = self.socket.local_addr();
        let now = self.now;
        let Some(connection) = self.get_mut(id) else {
            return;
        };
        let message = TaggedBytesMut {
            now,
            transport: TransportContext {
                local_addr,
                peer_addr: from,
                ecn: None,
                transport_protocol: TransportProtocol::UDP,
            },
            message: data,
        };
        if connection.core.handle_read(message).is_err() {
            return;
        }
        self.input(id);
    }

    fn timeouts(&mut self) {
        let now = self.now;
        while let Some(Reverse((at, id))) = self.timers.peek().copied() {
            if at > now {
                break;
            }
            self.timers.pop();
            let Some(connection) = self.get_mut(id) else {
                continue;
            };
            if connection.deadline != Some(at) {
                continue;
            }
            connection.deadline = None;
            connection.timed_out_at = Some(now);
            if let Err(error) = connection.core.handle_timeout(now) {
                tracing::debug!(request_id = %connection.context.request_id, %error, "voice peer timeout error");
            }
            self.input(id);
        }
    }

    fn reschedule(&mut self, id: PeerId) {
        let now = self.now;
        let Some(connection) = self.get_mut(id) else {
            return;
        };
        let mut next = connection.core.poll_timeout();
        if let Some(at) = next
            && at <= now
            && connection
                .timed_out_at
                .is_some_and(|last| now.saturating_duration_since(last) < MIN_IMMEDIATE_TIMEOUT)
        {
            next = Some(now + MIN_IMMEDIATE_TIMEOUT);
        }
        if next != connection.deadline {
            connection.deadline = next;
            if let Some(at) = next {
                self.timers.push(Reverse((at, id)));
            }
        }
    }

    async fn process(&mut self) {
        // Events and media reads may forward to receivers (marking them for
        // output), so drain inputs completely before collecting output.
        let mut index = 0;
        while index < self.inputs.len() {
            let id = self.inputs[index];
            index += 1;
            self.drain_input(id).await;
        }
        self.inputs.clear();
        let outputs = std::mem::take(&mut self.outputs);
        for &id in &outputs {
            let Some(connection) = connection_mut(&mut self.slots, id) else {
                continue;
            };
            connection.output = false;
            let mut learned = None;
            while let Some(message) = connection.core.poll_write() {
                let to = message.transport.peer_addr;
                if message.transport.transport_protocol != TransportProtocol::UDP
                    || to.port() == rtc::mdns::MDNS_PORT
                {
                    continue;
                }
                // Media to one peer goes to one address; learn each run once.
                if learned != Some(to) {
                    self.demux.learn(to, id, false);
                    learned = Some(to);
                }
                self.outbound.push((to, message.message));
            }
            self.reschedule(id);
        }
        let mut outputs = outputs;
        outputs.clear();
        self.outputs = outputs;
        let now = self.now;
        if !self.recovery.is_empty() {
            self.recovered
                .retain(|_, at| now.duration_since(*at) < RECOVERY_INTERVAL);
        }
        for recovery in self.recovery.drain(..) {
            let key = (
                Arc::as_ptr(&recovery.room) as usize,
                recovery.viewer,
                recovery.publisher,
                recovery.kind,
            );
            match self.recovered.entry(key) {
                std::collections::hash_map::Entry::Occupied(_) => continue,
                std::collections::hash_map::Entry::Vacant(entry) => entry.insert(now),
            };
            self.media.spawn(async move {
                request_keyframe(
                    &recovery.room,
                    recovery.viewer,
                    recovery.publisher,
                    recovery.kind,
                    recovery.epoch,
                    &recovery.active,
                )
                .await;
            });
        }
    }

    async fn drain_input(&mut self, id: PeerId) {
        let mut reads = std::mem::take(&mut self.media_reads);
        if let Some(connection) = self.get_mut(id) {
            connection.input = false;
            connection.events();
            connection.media(&mut reads);
        }
        for (kind, packet, arrived) in reads.drain(..) {
            self.forward(id, kind, packet, arrived).await;
        }
        self.media_reads = reads;
    }

    async fn flush(&mut self) {
        let mut sent = 0;
        while sent < self.outbound.len() {
            sent += self.socket.try_send(&self.outbound[sent..]);
            if sent < self.outbound.len()
                && tokio::time::timeout(SEND_WAIT, self.socket.writable())
                    .await
                    .is_err()
            {
                tracing::debug!(
                    dropped = self.outbound.len() - sent,
                    "voice shard send buffer full"
                );
                break;
            }
        }
        self.outbound.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transport_configuration_is_validated() {
        assert_eq!(Transport::parse(None, None).unwrap(), Transport::WebRtc);
        assert_eq!(
            Transport::parse(Some("webrtc"), None).unwrap(),
            Transport::WebRtc
        );
        assert_eq!(
            Transport::parse(Some("shard"), Some("3")).unwrap(),
            Transport::Shard { shards: 3 }
        );
        assert!(matches!(
            Transport::parse(Some(" shard "), None).unwrap(),
            Transport::Shard { shards } if (1..=DEFAULT_MAX_SHARDS).contains(&shards)
        ));
        for (transport, shards) in [
            (Some("Shard"), None),
            (Some(""), None),
            (Some("shard"), Some("0")),
            (Some("shard"), Some("65")),
            (Some("shard"), Some("two")),
            (None, Some("2")),
        ] {
            assert!(
                Transport::parse(transport, shards).is_err(),
                "{transport:?} {shards:?}"
            );
        }
    }
}
