//! One browser call: a supervisor that joins, negotiates with the SFU, keeps
//! signaling alive (resuming a lost socket within the grace period) and
//! retries transport failures with the native client's backoff. Replacement,
//! revocation, permission and protocol failures are final.
use super::{
    media::{self, Microphone, Output},
    socket::{self, Incoming, Socket},
    video,
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use thiscord_frontend::browser_audio::{ConnectionStats, RemoteSource};
use thiscord_frontend::browser_call::{
    self as rules, Activity, Answering, Backoff, Failure, FailureKind, Gate, PLAYBACK_SLOTS,
};
use thiscord_shared::{ChannelId, GuildId, RequestId, voice::*};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    RtcIceGatheringState, RtcPeerConnection, RtcPeerConnectionState, RtcRtpSender,
    RtcRtpTransceiver, RtcSdpType, RtcSessionDescriptionInit,
};

const TICK_MS: f64 = 100.0;
const STATS_MS: f64 = 200.0;
/// Self state doubles as the signaling keepalive the SFU expects within 20 s.
const STATE_REFRESH_MS: f64 = 2_000.0;
const HEARTBEAT_MS: f64 = 20_000.0;
const MEDIA_CONNECT_MS: f64 = 15_000.0;
const RESUME_WINDOW_MS: f64 = 20_000.0;

pub(super) fn now() -> f64 {
    js_sys::Date::now()
}

struct Receiver {
    slot: usize,
    track_id: String,
    mid: String,
}
/// Resources of one join attempt. Dropping it releases the membership
/// explicitly (`Leave`) and closes the peer.
pub(super) struct Live {
    socket: Socket,
    pc: RtcPeerConnection,
    own_slot: usize,
    can_speak: bool,
    publishing: bool,
    sender: Option<RtcRtpSender>,
    receivers: Vec<Receiver>,
    pub roster: Vec<Participant>,
    connected: bool,
    /// The SFU reads the Answer as the next frame after its Offer; nothing
    /// else may be sent before it.
    answered: bool,
    sent_state: Option<(bool, bool)>,
    sent_at: f64,
    /// Own-slot screen/camera senders and received video of this attempt.
    pub media: video::Media,
    /// Soundboard negotiated: offered by the SFU and opted into here.
    soundboard: bool,
}
impl Drop for Live {
    fn drop(&mut self) {
        self.socket.leave();
        self.pc.close();
    }
}
impl Live {
    pub(super) fn own(&self) -> Option<&Participant> {
        self.roster.iter().find(|m| m.slot == self.own_slot)
    }
    /// The Answer was sent; other events may follow it.
    pub(super) fn answered(&self) -> bool {
        self.answered
    }
    pub(super) fn send(&self, event: ClientEvent) {
        if self.answered {
            self.socket.send(event);
        }
    }
}

pub(super) struct Call {
    pub id: u64,
    pub guild_id: GuildId,
    pub(super) token: String,
    pub channel_id: Cell<ChannelId>,
    pub output: Output,
    pub microphone: RefCell<Option<Rc<Microphone>>>,
    /// Latched off when the server refuses speaking or capture fails, like the
    /// native client: a later grant needs an explicit rejoin.
    capture_allowed: Cell<bool>,
    pub live: RefCell<Option<Live>>,
    abort: RefCell<Option<futures_util::future::AbortHandle>>,
    closed: Cell<bool>,
    activation: Cell<Activity>,
    local_speaking: Cell<Activity>,
    remote: RefCell<[Activity; PLAYBACK_SLOTS]>,
    pub local_level: Cell<f32>,
    pub transmitting: Cell<bool>,
    reopening: Cell<u32>,
    /// Screen and camera capture; ends with the attempt, never restarted.
    pub publishing: RefCell<video::Publishing>,
    /// Whether the sender carries the WebAudio-processed microphone (boost)
    /// rather than the raw capture (used while the AudioContext cannot run).
    sent_processed: Cell<Option<bool>>,
    /// Latest `getStats` connection measurements.
    pub stats: Cell<Option<ConnectionStats>>,
    pub sounds: super::soundboard::Sounds,
}
impl Call {
    pub fn new(
        id: u64,
        guild_id: GuildId,
        channel_id: ChannelId,
        token: String,
        output: Output,
        microphone: Option<Microphone>,
    ) -> Rc<Self> {
        Rc::new(Self {
            id,
            guild_id,
            token,
            channel_id: Cell::new(channel_id),
            output,
            capture_allowed: Cell::new(microphone.is_some()),
            microphone: RefCell::new(microphone.map(Rc::new)),
            live: RefCell::new(None),
            abort: RefCell::new(None),
            closed: Cell::new(false),
            activation: Cell::default(),
            local_speaking: Cell::default(),
            remote: RefCell::new([Activity::default(); PLAYBACK_SLOTS]),
            local_level: Cell::new(0.0),
            transmitting: Cell::new(false),
            reopening: Cell::new(0),
            publishing: RefCell::default(),
            sent_processed: Cell::new(None),
            stats: Cell::new(None),
            sounds: Default::default(),
        })
    }
    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }
    /// Self or server deafen: no playback, no soundboard.
    pub fn deafened(&self) -> bool {
        super::settings().deafened
            || self
                .live
                .borrow()
                .as_ref()
                .and_then(Live::own)
                .is_some_and(|m| m.server_deafened)
    }
    pub fn soundboard(&self) -> bool {
        self.live
            .borrow()
            .as_ref()
            .is_some_and(|live| live.soundboard && live.connected)
    }
    /// Sends a client event after negotiation; false before the Answer.
    pub fn signal(&self, event: ClientEvent) -> bool {
        match self.live.borrow().as_ref() {
            Some(live) if live.answered => {
                live.socket.send(event);
                true
            }
            _ => false,
        }
    }
    pub fn start(self: &Rc<Self>) {
        let (handle, registration) = futures_util::future::AbortHandle::new_pair();
        *self.abort.borrow_mut() = Some(handle);
        let call = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let _ = futures_util::future::Abortable::new(supervise(call), registration).await;
        });
    }
    pub fn connected(&self) -> bool {
        self.live
            .borrow()
            .as_ref()
            .is_some_and(|live| live.connected)
    }
    pub fn remote_speaking(&self, slot: usize) -> bool {
        self.remote
            .borrow()
            .get(slot)
            .is_some_and(|a| a.active(rules::SPEAKING_HANGOVER_MS, now()))
    }
    /// Sends a signaling event on the current attempt after its Answer.
    pub(super) fn send(&self, event: ClientEvent) {
        if let Some(live) = self.live.borrow().as_ref() {
            live.send(event);
        }
    }
    fn end_attempt(&self) {
        // Screen and camera end with the media session; rejoining never
        // restarts them. Players rebind to the next attempt's tracks.
        video::release(self);
        video::unbind_all();
        let live = self.live.borrow_mut().take();
        drop(live);
        self.output.detach_all();
        *self.remote.borrow_mut() = [Activity::default(); PLAYBACK_SLOTS];
        self.transmitting.set(false);
        self.sent_processed.set(None);
        self.stats.set(None);
    }
    fn release_microphone(&self) {
        self.capture_allowed.set(false);
        if let Some(microphone) = self.microphone.borrow_mut().take() {
            microphone.stop();
        }
    }
    /// Idempotent teardown: Leave, peer, microphone, playback.
    pub fn shutdown(&self) {
        if self.closed.replace(true) {
            return;
        }
        if let Some(abort) = self.abort.borrow_mut().take() {
            abort.abort();
        }
        self.end_attempt();
        self.release_microphone();
        self.output.close();
        self.local_level.set(0.0);
    }
    /// Applies self controls, server restrictions and volumes immediately and
    /// sends changed self state. Safe to call from commands at any time.
    pub fn apply(&self) {
        let settings = super::settings();
        let pressed = super::pressed_now();
        let hidden = web_sys::window()
            .and_then(|w| w.document())
            .is_some_and(|d| d.hidden());
        let live = self.live.borrow();
        let own = live.as_ref().and_then(Live::own);
        let microphone = self.microphone.borrow().clone();
        // The WebAudio send path (boost, meters) works only while the context
        // runs; otherwise the raw capture is sent and gated directly.
        let processed = self.output.running() && microphone.as_ref().is_some_and(|m| m.metered());
        // Hidden tabs run timers about once per second, too coarse to gate
        // speech, and a stopped meter cannot open the gate: both fail open.
        let metered = !hidden && processed;
        let voice_open = !metered
            || self
                .activation
                .get()
                .active(rules::ACTIVATION_HOLD_MS, now());
        let gate = Gate {
            can_speak: live.as_ref().is_some_and(|l| l.publishing),
            server_muted: own.is_some_and(|m| m.server_muted),
            server_deafened: own.is_some_and(|m| m.server_deafened),
            pressed,
            voice_open,
        };
        let sending = gate.sending(&settings);
        if let Some(microphone) = &microphone {
            microphone.set_boost(settings.input_gain);
            microphone.set_sending(sending, processed);
        }
        // Without a meter the indicator follows the send gate alone.
        let speaking = !metered
            || self
                .local_speaking
                .get()
                .active(rules::SPEAKING_HANGOVER_MS, now());
        self.transmitting.set(sending && speaking);
        let deafened = settings.deafened || own.is_some_and(|m| m.server_deafened);
        self.output.set_master(settings.output_volume, deafened);
        for slot in 0..PLAYBACK_SLOTS {
            let Some((source, member)) = RemoteSource::from_slot(slot) else {
                continue;
            };
            let gain = live
                .as_ref()
                .and_then(|live| live.roster.iter().find(|m| m.slot == member))
                .map_or(1.0, |m| {
                    super::stream_gain(self.guild_id, m.account_id, source)
                });
            self.output.set_stream_gain(source, member, gain);
        }
        // Swap the sent track when the processed path starts or stops working.
        if let (Some(microphone), Some(sender)) =
            (&microphone, live.as_ref().and_then(|l| l.sender.clone()))
            && self.sent_processed.get() != Some(processed)
        {
            self.sent_processed.set(Some(processed));
            let promise = sender.replace_track(Some(microphone.track(processed)));
            wasm_bindgen_futures::spawn_local(async move {
                let _ = JsFuture::from(promise).await;
            });
        }
        drop(live);
        let state = (settings.muted, settings.deafened);
        if let Some(live) = self.live.borrow_mut().as_mut()
            && live.answered
            && (live.sent_state != Some(state) || now() - live.sent_at >= STATE_REFRESH_MS)
        {
            live.socket.send(ClientEvent::State {
                muted: state.0,
                deafened: state.1,
            });
            live.sent_state = Some(state);
            live.sent_at = now();
        }
    }
    /// Reopens the microphone with new capture settings and swaps it into the
    /// current sender without renegotiating.
    pub fn reopen_microphone(self: &Rc<Self>) {
        if !self.capture_allowed.get() || self.closed.get() {
            return;
        }
        let generation = self.reopening.get().wrapping_add(1);
        self.reopening.set(generation);
        let call = self.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let capture = rules::Capture::from_settings(&super::settings());
            let opened = Microphone::open(&capture, call.output.context()).await;
            if call.closed.get() || call.reopening.get() != generation {
                if let Ok(microphone) = opened {
                    microphone.stop();
                }
                return;
            }
            match opened {
                Ok(microphone) => {
                    let sender = call
                        .live
                        .borrow()
                        .as_ref()
                        .and_then(|live| live.sender.clone());
                    let processed = call.output.running() && microphone.metered();
                    if let Some(sender) = sender {
                        let track = microphone.track(processed);
                        let _ = JsFuture::from(sender.replace_track(Some(track))).await;
                        call.sent_processed.set(Some(processed));
                    }
                    if call.closed.get() || call.reopening.get() != generation {
                        microphone.stop();
                        return;
                    }
                    if let Some(old) = call.microphone.borrow_mut().replace(Rc::new(microphone)) {
                        old.stop();
                    }
                    call.apply();
                }
                Err(error) => super::set_notice(error.notice()),
            }
        });
    }
    fn tick(&self, stats: Option<media::Stats>) {
        let settings = super::settings();
        let time = now();
        self.output.check(time);
        let level = self
            .microphone
            .borrow()
            .as_ref()
            .and_then(|m| m.level())
            .unwrap_or(0.0);
        self.local_level.set(level);
        let threshold = settings.activation_threshold;
        let mut activation = self.activation.get();
        activation.observe(level, threshold, rules::ACTIVATION_HOLD_MS, time);
        self.activation.set(activation);
        let mut speaking = self.local_speaking.get();
        speaking.observe(level, threshold, rules::SPEAKING_HANGOVER_MS, time);
        self.local_speaking.set(speaking);
        // Remote speaking: WebAudio meters while the graph runs (every tick),
        // else `getStats` inbound `audioLevel` (every stats pass).
        let mut metered = [false; PLAYBACK_SLOTS];
        {
            let mut remote = self.remote.borrow_mut();
            for (slot, activity) in remote.iter_mut().enumerate() {
                if let Some(level) = self.output.level(slot) {
                    metered[slot] = true;
                    activity.observe(
                        level,
                        rules::REMOTE_SPEAKING_LEVEL,
                        rules::SPEAKING_HANGOVER_MS,
                        time,
                    );
                }
            }
        }
        if let Some(stats) = stats
            && let Some(live) = self.live.borrow().as_ref()
        {
            self.stats.set(Some(stats.connection));
            let mut remote = self.remote.borrow_mut();
            for (track_id, mid, level) in stats.levels {
                let slot = live
                    .receivers
                    .iter()
                    .find(|r| {
                        track_id.as_deref() == Some(r.track_id.as_str())
                            || mid.as_deref() == Some(r.mid.as_str())
                    })
                    .map(|r| r.slot)
                    .filter(|slot| !metered.get(*slot).copied().unwrap_or(true));
                if let Some(activity) = slot.and_then(|slot| remote.get_mut(slot)) {
                    activity.observe(
                        level,
                        rules::REMOTE_SPEAKING_LEVEL,
                        rules::SPEAKING_HANGOVER_MS,
                        time,
                    );
                }
            }
        }
        self.apply();
    }
}

enum Ending {
    Failed(Failure),
    Moved(ChannelId),
}
impl From<Failure> for Ending {
    fn from(failure: Failure) -> Self {
        Self::Failed(failure)
    }
}

fn request_id() -> Option<RequestId> {
    let mut bytes = [0_u8; 16];
    web_sys::window()?
        .crypto()
        .ok()?
        .get_random_values_with_u8_array(&mut bytes)
        .ok()?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.iter().map(|b| format!("{b:02x}")).collect::<String>();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
    .parse()
    .ok()
}

async fn supervise(call: Rc<Call>) {
    let mut backoff = Backoff::default();
    let mut call_id: Option<RequestId> = None;
    loop {
        if call.closed.get() {
            return;
        }
        super::update_voice(&call, |voice| {
            *voice = VoiceStatus {
                channel_id: Some(call.channel_id.get()),
                message: "Connecting to voice…".into(),
                ..Default::default()
            }
        });
        let mut connected_at = None;
        let ending = attempt(&call, &mut call_id, &mut connected_at).await;
        call.end_attempt();
        let mut stable = connected_at.is_some_and(|at| now() - at >= rules::STABLE_CALL_MS);
        let failure = match ending {
            Ending::Moved(destination) => {
                call.channel_id.set(destination);
                call_id = None;
                stable = true;
                Failure::temporary(
                    "Moved by a moderator. Reconnecting with your existing mute/deafen settings.",
                )
            }
            Ending::Failed(failure) => failure,
        };
        if !failure.retryable {
            super::finish(&call, failure.message);
            return;
        }
        let delay = backoff.next_ms(stable, (js_sys::Math::random() * 501.0) as u32);
        super::update_voice(&call, |voice| {
            *voice = VoiceStatus {
                channel_id: Some(call.channel_id.get()),
                message: format!(
                    "Reconnecting in {:.1}s - {}",
                    f64::from(delay) / 1000.0,
                    failure.message
                ),
                ..Default::default()
            }
        });
        gloo_timers::future::TimeoutFuture::new(delay).await;
    }
}

fn parse(text: &str) -> Result<ServerEvent, Failure> {
    let frame: ServerFrame =
        serde_json::from_str(text).map_err(|_| Failure::final_("Invalid voice event"))?;
    if frame.version != VOICE_VERSION {
        return Err(Failure::final_("Unsupported voice version"));
    }
    Ok(frame.event)
}
fn received(incoming: Option<Incoming>, timeout: &'static str) -> Result<ServerEvent, Failure> {
    match incoming {
        Some(Incoming::Text(text)) => parse(&text),
        Some(Incoming::Closed(code)) => Err(Failure::closed(code)),
        Some(Incoming::Open) => Err(Failure::final_("Invalid voice event")),
        Some(Incoming::Failed) => Err(Failure::temporary("Voice socket failed")),
        None => Err(Failure::temporary(timeout)),
    }
}

async fn attempt(
    call: &Rc<Call>,
    call_id: &mut Option<RequestId>,
    connected_at: &mut Option<f64>,
) -> Ending {
    let url = format!("{}{}", crate::account_client::socket_base(), VOICE_PATH);
    let socket = match Socket::open(&url) {
        Ok(socket) => socket,
        Err(failure) => return failure.into(),
    };
    if let Err(failure) = socket.opened(10_000).await {
        return failure.into();
    }
    let reconnect = call_id.is_some();
    let Some(id) = call_id.or_else(request_id) else {
        return Failure::final_("Secure random numbers are unavailable").into();
    };
    *call_id = Some(id);
    socket.send(ClientEvent::Join {
        token: call.token.clone(),
        guild_id: call.guild_id,
        channel_id: call.channel_id.get(),
        attempt: Some(JoinAttempt {
            call_id: id,
            reconnect,
        }),
    });
    let inbox = socket.inbox();
    // Until the attempt owns a Live (whose drop sends Leave), release the
    // membership explicitly on every early exit after Join.
    let first = match received(socket::next(&inbox, 20_000).await, "Voice offer timed out") {
        Ok(event) => event,
        Err(failure) => {
            socket.leave();
            return failure.into();
        }
    };
    let (sdp, own_slot, can_speak, ice_servers, resume_token, soundboard) = match first {
        ServerEvent::Offer {
            sdp,
            slot,
            can_speak,
            ice_servers,
            resume_token,
            soundboard,
            ..
        } if slot < ROOM_CAPACITY => (sdp, slot, can_speak, ice_servers, resume_token, soundboard),
        ServerEvent::Replaced {} => return Failure::replaced().into(),
        ServerEvent::Error { error } => return Failure::server(&error).into(),
        _ => {
            socket.leave();
            return Failure::final_("Invalid voice offer").into();
        }
    };
    let can_speak = can_speak && call.capture_allowed.get();
    if !can_speak {
        call.release_microphone();
    }
    let pc = match media::peer(&ice_servers) {
        Ok(pc) => pc,
        Err(_) => {
            socket.leave();
            return Failure::final_("Cannot initialize voice media connection").into();
        }
    };
    drop(ice_servers);
    let microphone = call.microphone.borrow().clone();
    *call.live.borrow_mut() = Some(Live {
        socket,
        pc: pc.clone(),
        own_slot,
        can_speak,
        publishing: false,
        sender: None,
        receivers: vec![],
        roster: vec![],
        connected: false,
        answered: false,
        sent_state: None,
        sent_at: 0.0,
        media: Default::default(),
        soundboard,
    });
    let answer = match negotiate(call, &pc, &sdp, own_slot, can_speak, microphone.as_deref()).await
    {
        Ok(answer) => answer,
        Err(failure) => return failure.into(),
    };
    if let Some(live) = call.live.borrow_mut().as_mut() {
        live.answered = true;
        live.socket.send(ClientEvent::Answer {
            browser_publisher: true,
            packet_redundancy: false,
            remote_audio: false,
            sdp: answer,
            // Viewing demand, ScreenTarget and keyframe recovery; browsers
            // publish one encoding (no layers) and decode baseline only.
            screen_feedback: true,
            screen_subscriptions: true,
            screen_layers: false,
            video_diagnostics: false,
            // Opt in whenever the SFU offers it (guild calls only).
            soundboard,
            screen_profiles: false,
        });
    }
    run(call, inbox, resume_token, connected_at).await
}

async fn negotiate(
    call: &Call,
    pc: &RtcPeerConnection,
    offer: &str,
    own_slot: usize,
    can_speak: bool,
    microphone: Option<&Microphone>,
) -> Result<String, Failure> {
    #[derive(serde::Deserialize)]
    struct Description {
        #[serde(rename = "type")]
        kind: String,
        sdp: String,
    }
    let offer: Description =
        serde_json::from_str(offer).map_err(|_| Failure::final_("Invalid voice offer"))?;
    let sections = rules::sections(&offer.sdp);
    if offer.kind != "offer" || !rules::valid_offer(&sections) {
        return Err(Failure::final_("Unsupported voice offer"));
    }
    let description = RtcSessionDescriptionInit::new(RtcSdpType::Offer);
    description.set_sdp(&offer.sdp);
    JsFuture::from(pc.set_remote_description(&description))
        .await
        .map_err(|_| Failure::final_("Cannot accept the voice server offer"))?;
    let publish = can_speak && microphone.is_some();
    let support = video::support();
    let answering = Answering {
        microphone: publish,
        media: can_speak,
        h264_send: support.encode,
        h264_receive: support.decode,
    };
    let opus = media::opus_codecs();
    let mut sender = None;
    // The SFU rejects a RED microphone from browser publishers.
    let mut opus_only = true;
    // Plain baseline H.264 only, without RTX/RED/FEC.
    let mut h264_only = true;
    let mut receivers = Vec::new();
    let mut video_media = video::Media::new(support);
    for transceiver in pc.get_transceivers().iter() {
        let transceiver: RtcRtpTransceiver = transceiver.unchecked_into();
        let Some(mid) = transceiver.mid() else {
            continue;
        };
        let Some((kind, slot)) = rules::track_for_mid(&sections, &mid) else {
            media::set_direction(&transceiver, rules::Direction::Inactive.as_str());
            continue;
        };
        let direction = rules::direction(kind, slot, own_slot, answering);
        // Every audio m-line, even inactive ones, offers plain Opus only.
        let preferred = if kind.is_video() {
            direction == rules::Direction::Inactive || video::prefer_h264(&transceiver)
        } else {
            opus.as_ref()
                .is_some_and(|codecs| media::prefer_codecs(&transceiver, codecs))
        };
        if direction == rules::Direction::SendRecv && !kind.is_video() {
            opus_only &= preferred;
        }
        if kind.is_video() {
            h264_only &= preferred;
        }
        if direction == rules::Direction::SendRecv
            && kind == MediaKind::Microphone
            && let Some(microphone) = microphone
        {
            let track_sender = transceiver.sender();
            // The processed (boosted) track while WebAudio runs; `apply` swaps
            // tracks when that changes.
            let processed = call.output.running() && microphone.metered();
            JsFuture::from(track_sender.replace_track(Some(microphone.track(processed))))
                .await
                .map_err(|_| Failure::final_("Cannot attach the microphone"))?;
            call.sent_processed.set(Some(processed));
            sender = Some(track_sender);
        }
        if direction == rules::Direction::SendRecv
            && let Some(index) = video::sender_index(kind)
        {
            // Idle until Share screen or the camera attaches a track.
            video_media.senders[index] = Some(transceiver.sender());
        }
        if !media::set_direction(&transceiver, direction.as_str()) {
            return Err(Failure::final_("Cannot configure voice media"));
        }
        if let Some(playback) = rules::playback_slot(kind, slot, own_slot)
            && let Some((source, member)) = RemoteSource::from_slot(playback)
        {
            let track = transceiver.receiver().track();
            receivers.push(Receiver {
                slot: playback,
                track_id: track.id(),
                mid,
            });
            call.output.play_remote(source, member, &track);
            if source == RemoteSource::SystemAudio {
                // Plays only while this member's screen is watched (`video::tick`).
                video_media
                    .shared_audio
                    .push((member, Clone::clone(&track)));
            }
        } else if kind.is_video() && direction == rules::Direction::RecvOnly {
            video_media.remote.push(video::Remote {
                kind,
                slot,
                mid,
                track: transceiver.receiver().track(),
            });
        }
    }
    if let Some(live) = call.live.borrow_mut().as_mut() {
        live.publishing = sender.is_some();
        live.sender = sender;
        live.receivers = receivers;
        live.media = video_media;
    }
    let answer: RtcSessionDescriptionInit = JsFuture::from(pc.create_answer())
        .await
        .map_err(|_| Failure::final_("Cannot create a voice answer"))?
        .unchecked_into();
    if !opus_only || !h264_only {
        // Without setCodecPreferences, drop RED from audio and every format
        // but baseline H.264 from video sections of the answer instead.
        let mut created = js_sys::Reflect::get(&answer, &"sdp".into())
            .ok()
            .and_then(|sdp| sdp.as_string())
            .ok_or_else(|| Failure::final_("Cannot create a voice answer"))?;
        if !opus_only {
            created = rules::strip_red(&created);
        }
        if !h264_only {
            created = thiscord_frontend::browser_video::restrict_video(&created);
        }
        answer.set_sdp(&created);
    }
    JsFuture::from(pc.set_local_description(&answer))
        .await
        .map_err(|_| Failure::final_("Cannot set the voice answer"))?;
    // The SFU does not trickle candidates: send the answer once gathering ends.
    let started = now();
    while pc.ice_gathering_state() != RtcIceGatheringState::Complete {
        if now() - started > 8_000.0 {
            break;
        }
        gloo_timers::future::TimeoutFuture::new(50).await;
    }
    let local = pc
        .local_description()
        .ok_or_else(|| Failure::temporary("Missing voice answer"))?;
    let sdp = local.sdp();
    if pc.ice_gathering_state() != RtcIceGatheringState::Complete && !sdp.contains("a=candidate:") {
        return Err(Failure::temporary("ICE gathering timed out"));
    }
    Ok(serde_json::json!({"type": "answer", "sdp": sdp}).to_string())
}

async fn run(
    call: &Rc<Call>,
    mut inbox: socket::SharedInbox,
    resume_token: Option<String>,
    connected_at: &mut Option<f64>,
) -> Ending {
    let answered = now();
    let mut heard = now();
    let mut next_tick = now();
    let mut next_stats = now();
    let mut sequence = 0_u64;
    loop {
        let wait = (next_tick - now()).clamp(0.0, TICK_MS) as u32;
        let incoming = socket::next(&inbox, wait).await;
        let mut failure = None;
        match incoming {
            Some(Incoming::Text(text)) => {
                heard = now();
                match parse(&text) {
                    Ok(event) => {
                        if let Some(ending) = handle(call, event) {
                            return ending;
                        }
                    }
                    Err(error) => return error.into(),
                }
            }
            Some(Incoming::Closed(code)) => failure = Some(Failure::closed(code)),
            Some(Incoming::Failed) => failure = Some(Failure::temporary("Voice socket failed")),
            Some(Incoming::Open) | None => {}
        }
        if failure.is_none() && now() - heard > HEARTBEAT_MS {
            failure = Some(Failure::temporary("Voice signaling timed out"));
        }
        let peer_state = call
            .live
            .borrow()
            .as_ref()
            .map(|live| live.pc.connection_state());
        let peer_failed = matches!(
            peer_state,
            None | Some(RtcPeerConnectionState::Failed | RtcPeerConnectionState::Closed)
        );
        if let Some(failure) = failure {
            if !failure.resumable(resume_token.is_some(), peer_failed) {
                return failure.into();
            }
            match resume(
                call,
                resume_token.as_deref().unwrap_or_default(),
                &mut sequence,
            )
            .await
            {
                Ok(next) => {
                    inbox = next;
                    heard = now();
                    continue;
                }
                Err(failure) => return failure.into(),
            }
        }
        if now() < next_tick {
            continue;
        }
        next_tick = now() + TICK_MS;
        if peer_failed {
            return Failure::temporary("Voice connection lost").into();
        }
        if peer_state == Some(RtcPeerConnectionState::Connected) {
            let first = call
                .live
                .borrow_mut()
                .as_mut()
                .is_some_and(|live| !std::mem::replace(&mut live.connected, true));
            if first {
                *connected_at = Some(now());
                let (can_speak, soundboard) = call
                    .live
                    .borrow()
                    .as_ref()
                    .map_or((false, false), |l| (l.can_speak, l.soundboard));
                super::update_voice(call, |voice| {
                    voice.connected = true;
                    voice.soundboard = soundboard;
                    voice.message = if can_speak {
                        "Connected"
                    } else {
                        "Connected as listener. Leave and join again to enable newly granted microphone access."
                    }
                    .into();
                });
            }
        } else if connected_at.is_none() && now() - answered > MEDIA_CONNECT_MS {
            return Failure::temporary("Media connection timed out").into();
        }
        let stats = if now() >= next_stats {
            next_stats = now() + STATS_MS;
            let pc = call.live.borrow().as_ref().map(|live| live.pc.clone());
            match pc {
                Some(pc) => Some(media::stats(&pc).await),
                None => None,
            }
        } else {
            None
        };
        let video_stats = if video::stats_due(call) {
            let pc = call.live.borrow().as_ref().map(|live| live.pc.clone());
            match pc {
                Some(pc) => Some(video::inbound(&pc).await),
                None => None,
            }
        } else {
            None
        };
        video::tick(call, video_stats);
        call.tick(stats);
    }
}

/// Server events after negotiation. Returns how the attempt ends, if it does.
fn handle(call: &Rc<Call>, event: ServerEvent) -> Option<Ending> {
    match event {
        ServerEvent::Participants { members } => {
            if members.len() > ROOM_CAPACITY || members.iter().any(|m| m.slot >= ROOM_CAPACITY) {
                return Some(Failure::final_("Invalid participant list").into());
            }
            let own_slot = {
                let mut live = call.live.borrow_mut();
                let live = live.as_mut()?;
                if !members.iter().any(|m| m.slot == live.own_slot) {
                    return Some(Failure::final_("Missing own participant").into());
                }
                live.roster = members.clone();
                live.own_slot
            };
            super::update_voice(call, |voice| {
                voice.participants = members;
                voice.own_slot = Some(own_slot);
            });
            call.apply();
            None
        }
        ServerEvent::Moved { channel_id } => Some(if channel_id == call.channel_id.get() {
            Failure::final_("Invalid move destination").into()
        } else {
            Ending::Moved(channel_id)
        }),
        ServerEvent::Revoked {} => Some(Failure::revoked().into()),
        ServerEvent::Replaced {} => Some(Failure::replaced().into()),
        ServerEvent::ResumeUnavailable {} => Some(Failure::resume_unavailable().into()),
        ServerEvent::Error { error } => Some(Failure::server(&error).into()),
        ServerEvent::ScreenTarget {
            kind,
            epoch,
            bitrate,
            ..
        } => {
            video::target(call, kind, epoch, bitrate);
            None
        }
        // Only delivered after this client opted in (`Answer.soundboard`).
        ServerEvent::Soundboard {
            sound_id,
            account_id,
        } => {
            super::soundboard::played(call, sound_id, account_id);
            None
        }
        // Plain Opus publishers have no RED privacy boundary to refresh;
        // browsers answer keyframe requests as RTCP PLI, and admin events are
        // never negotiated here. MediaDiagnostics is ignored.
        _ => None,
    }
}

/// Reattaches signaling to the same media session (native recovery rules):
/// bounded retries for 20 s, never a Join, never after ResumeUnavailable.
async fn resume(
    call: &Rc<Call>,
    resume_token: &str,
    sequence: &mut u64,
) -> Result<socket::SharedInbox, Failure> {
    super::update_voice(call, |voice| {
        voice.message = "Recovering voice signaling...".into();
    });
    let started = now();
    let mut failures = 0_u32;
    loop {
        *sequence = sequence
            .checked_add(1)
            .ok_or_else(|| Failure::final_("Voice recovery sequence exhausted"))?;
        match super::within(5_000, resume_once(call, resume_token, *sequence)).await {
            Some(Ok(socket)) => {
                let inbox = socket.inbox();
                {
                    let mut guard = call.live.borrow_mut();
                    let live = guard
                        .as_mut()
                        .ok_or_else(|| Failure::final_("Voice call ended"))?;
                    // The replaced socket closes without Leave: the session continues.
                    live.socket = socket;
                    live.sent_state = None;
                }
                super::update_voice(call, |voice| {
                    if voice.connected {
                        voice.message = "Connected".into();
                    }
                });
                return Ok(inbox);
            }
            Some(Err(failure))
                if !failure.retryable || failure.kind == FailureKind::ResumeUnavailable =>
            {
                return Err(failure);
            }
            _ => {}
        }
        failures += 1;
        if now() - started > RESUME_WINDOW_MS {
            return Err(Failure::final_(
                "Voice signaling recovery expired. Join again to reconnect.",
            ));
        }
        gloo_timers::future::TimeoutFuture::new(rules::resume_delay_ms(
            failures,
            (js_sys::Math::random() * 255.0) as u8,
        ))
        .await;
    }
}
async fn resume_once(call: &Call, resume_token: &str, sequence: u64) -> Result<Socket, Failure> {
    let socket = Socket::open(&format!(
        "{}{}",
        crate::account_client::socket_base(),
        VOICE_PATH
    ))?;
    socket.opened(5_000).await?;
    socket.send(ClientEvent::Resume {
        token: call.token.clone(),
        guild_id: call.guild_id,
        channel_id: call.channel_id.get(),
        resume_token: resume_token.to_owned(),
        sequence,
    });
    match received(
        socket::next(&socket.inbox(), 5_000).await,
        "Voice resume timed out",
    )? {
        ServerEvent::Resumed {} => Ok(socket),
        ServerEvent::ResumeUnavailable {} => Err(Failure::resume_unavailable()),
        ServerEvent::Replaced {} => Err(Failure::replaced()),
        ServerEvent::Error { error } => Err(Failure::server(&error)),
        _ => Err(Failure::final_("Unexpected resume response")),
    }
}
