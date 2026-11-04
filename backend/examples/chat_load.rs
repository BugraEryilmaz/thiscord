//! Headless text-chat load generator for the disposable `voice_load_server`.
//! Opens each simulated user's chat, friends and notification sockets like the
//! client does, posts messages over HTTP at a fixed rate and measures delivery
//! to every subscribed socket. See docs/chat-load.md.
#[path = "voice_load/common.rs"]
mod common;
// Shared bitmap/percentile helpers; the voice-only stream types stay unused here.
#[allow(dead_code)]
#[path = "voice_load/metrics.rs"]
mod metrics;

use common::{Args, Result};
use futures_util::{SinkExt, StreamExt};
use metrics::{Bits, percentile};
use serde_json::json;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering::Relaxed},
    },
    time::{Duration, Instant},
};
use thiscord_shared::{
    ChannelId, ClientMessageId, GuildId,
    chat::{self, ChatRequest},
    friends, notifications,
};
use tokio::sync::watch;
use tokio_tungstenite::{
    MaybeTlsStream, WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;
const PREFIX: &str = "chatload";
/// Same per-account send budget as the server's sensitive chat limiter (30/min).
const MAX_SENDS_PER_ACCOUNT_PER_MINUTE: f64 = 30.0;

struct Config {
    http: url::Url,
    ws: url::Url,
    secret: String,
    users: usize,
    channels: usize,
    rate: f64,
    seconds: usize,
    late_ms: u64,
    full_client: bool,
    epoch: Instant,
    run: String,
}
impl Config {
    fn channel(&self, user: usize) -> ChannelId {
        ChannelId::from_uuid(common::id("text", user % self.channels))
    }
    fn messages(&self) -> usize {
        (self.rate * self.seconds as f64).ceil() as usize
    }
}

fn origins(base: &str, insecure: bool) -> Result<(url::Url, url::Url)> {
    let url = url::Url::parse(base).map_err(|_| "Invalid --url")?;
    if url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err("--url must be a server origin without credentials, path, query or fragment");
    }
    let scheme = match url.scheme() {
        "https" => "wss",
        "http" if insecure => "ws",
        _ => return Err("Use https, or explicitly pass --allow-insecure for a trusted LAN"),
    };
    let mut ws = url.clone();
    ws.set_scheme(scheme).map_err(|_| "Invalid URL scheme")?;
    Ok((url, ws))
}

/// Per receiving user: unique messages, latency histogram and socket health.
struct Receiver {
    received: Bits,
    histogram: Vec<u64>,
    late: u64,
    duplicates: u64,
    invalid: u64,
    failed: Option<&'static str>,
    events: u64,
}
impl Receiver {
    fn new(messages: usize) -> Self {
        Self {
            received: Bits::new(messages),
            histogram: vec![0; 10_002],
            late: 0,
            duplicates: 0,
            invalid: 0,
            failed: None,
            events: 0,
        }
    }
}

#[derive(Default)]
struct Sends {
    ok: Bits,
    attempted: u64,
    errors: std::collections::BTreeMap<String, u64>,
    histogram: Vec<u64>,
}

async fn connect(config: &Config, path: &str) -> Result<Socket> {
    let mut url = config.ws.clone();
    url.set_path(path);
    let mut request = url
        .as_str()
        .into_client_request()
        .map_err(|_| "Invalid WebSocket URL")?;
    request
        .headers_mut()
        .insert("origin", common::ORIGIN.parse().unwrap());
    let (socket, _) = tokio::time::timeout(
        Duration::from_secs(10),
        tokio_tungstenite::connect_async(request),
    )
    .await
    .map_err(|_| "WebSocket connection timeout")?
    .map_err(|_| "WebSocket connection failed (capacity, address or TLS)")?;
    Ok(socket)
}
async fn send_json(socket: &mut Socket, value: serde_json::Value) -> Result<()> {
    tokio::time::timeout(
        Duration::from_secs(2),
        socket.send(Message::Text(value.to_string().into())),
    )
    .await
    .map_err(|_| "Socket send timeout")?
    .map_err(|_| "Socket send failed")
}
async fn next_text(socket: &mut Socket) -> Result<String> {
    loop {
        match socket.next().await {
            Some(Ok(Message::Text(text))) => return Ok(text.to_string()),
            Some(Ok(Message::Ping(data))) => socket
                .send(Message::Pong(data))
                .await
                .map_err(|_| "Pong failed")?,
            Some(Ok(Message::Pong(_) | Message::Binary(_) | Message::Frame(_))) => {}
            _ => return Err("Socket closed"),
        }
    }
}

/// The chat socket, subscribed like the desktop client (voice roster + member list).
async fn chat_socket(config: &Config, user: usize) -> Result<Socket> {
    let mut socket = connect(config, chat::SOCKET_PATH).await?;
    let frame = |event: chat::ClientEvent| {
        serde_json::to_value(chat::ClientFrame {
            version: chat::SOCKET_VERSION,
            event,
        })
        .unwrap()
    };
    send_json(
        &mut socket,
        frame(chat::ClientEvent::Connect {
            token: common::token(&config.secret, user),
            guild_updates: true,
        }),
    )
    .await?;
    let ready = |text: &str| serde_json::from_str::<chat::ServerFrame>(text).map(|f| f.event);
    if !matches!(
        ready(&next_text(&mut socket).await?),
        Ok(chat::ServerEvent::Authenticated {})
    ) {
        return Err("Chat socket authentication rejected");
    }
    send_json(
        &mut socket,
        frame(chat::ClientEvent::Subscribe {
            subscription: 1,
            guild_id: Some(GuildId::from_uuid(common::id("guild", 0))),
            channel_id: Some(config.channel(user)),
            voice_roster: true,
            members: true,
        }),
    )
    .await?;
    loop {
        match ready(
            &tokio::time::timeout(Duration::from_secs(20), next_text(&mut socket))
                .await
                .map_err(|_| "Subscribe timeout")??,
        ) {
            Ok(chat::ServerEvent::Subscribed { .. }) => return Ok(socket),
            Ok(chat::ServerEvent::Update { .. }) => {}
            _ => return Err("Chat subscription rejected"),
        }
    }
}

fn record(config: &Config, receiver: &Mutex<Receiver>, text: &str) {
    let Ok(frame) = serde_json::from_str::<chat::ServerFrame>(text) else {
        receiver.lock().unwrap().invalid += 1;
        return;
    };
    let mut r = receiver.lock().unwrap();
    r.events += 1;
    let event = match frame.event {
        chat::ServerEvent::Update { event, .. } => *event,
        chat::ServerEvent::Revoked {} => {
            r.failed.get_or_insert("revoked");
            return;
        }
        chat::ServerEvent::Error { .. } => {
            r.failed.get_or_insert("server error event");
            return;
        }
        _ => return,
    };
    let chat::ServerEvent::Message { message, .. } = event else {
        if matches!(event, chat::ServerEvent::Error { .. }) {
            r.failed.get_or_insert("subscription error event");
        }
        return;
    };
    let mut parts = message.content.split(' ');
    let (Some(PREFIX), Some(run), Some(Ok(sequence)), Some(Ok(sent_us))) = (
        parts.next(),
        parts.next(),
        parts.next().map(str::parse::<usize>),
        parts.next().map(str::parse::<u64>),
    ) else {
        r.invalid += 1;
        return;
    };
    if run != config.run || sequence >= config.messages() {
        r.invalid += 1;
        return;
    }
    let now = config.epoch.elapsed().as_micros() as u64;
    let delay = now.saturating_sub(sent_us) / 1000;
    if !r.received.insert(sequence) {
        r.duplicates += 1;
        return;
    }
    r.late += u64::from(delay > config.late_ms);
    r.histogram[delay.min(10_001) as usize] += 1;
}

/// Holds one user's sockets until `stop`; heartbeats every 15 seconds.
async fn user(
    config: Arc<Config>,
    index: usize,
    receiver: Arc<Mutex<Receiver>>,
    connected: Arc<AtomicU64>,
    mut stop: watch::Receiver<bool>,
    join_slot: tokio::sync::OwnedSemaphorePermit,
) {
    let mut join_slot = Some(join_slot);
    let result: Result<()> = async {
        let mut chat_socket = chat_socket(&config, index).await?;
        let mut extras = Vec::new();
        if config.full_client {
            let mut friends_socket = connect(&config, friends::FRIENDS_SOCKET_PATH).await?;
            send_json(
                &mut friends_socket,
                serde_json::to_value(friends::ClientFrame {
                    version: friends::VERSION,
                    event: friends::ClientEvent::Connect {
                        token: common::token(&config.secret, index),
                    },
                })
                .unwrap(),
            )
            .await?;
            let mut alerts = connect(&config, notifications::SOCKET_PATH).await?;
            send_json(
                &mut alerts,
                serde_json::to_value(notifications::ClientFrame {
                    version: notifications::VERSION,
                    event: notifications::ClientEvent::Connect {
                        token: common::token(&config.secret, index),
                        kinds: notifications::Kind::ALL.to_vec(),
                    },
                })
                .unwrap(),
            )
            .await?;
            extras.push((
                friends_socket,
                json!({"version": friends::VERSION, "event": {"type": "ping"}}),
            ));
            extras.push((
                alerts,
                json!({"version": notifications::VERSION, "event": {"type": "ping"}}),
            ));
        }
        connected.fetch_add(1, Relaxed);
        join_slot.take();
        let ping = json!({"version": chat::SOCKET_VERSION, "event": {"type": "ping"}});
        let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
        heartbeat.tick().await;
        let (mut extra_a, mut extra_b) = {
            let mut iter = extras.into_iter();
            (iter.next(), iter.next())
        };
        async fn drain(socket: &mut Option<(Socket, serde_json::Value)>) -> Result<()> {
            match socket {
                Some((socket, _)) => next_text(socket).await.map(|_| ()),
                None => std::future::pending().await,
            }
        }
        loop {
            tokio::select! {
                _ = stop.changed() => return Ok(()),
                text = next_text(&mut chat_socket) => record(&config, &receiver, &text?),
                result = drain(&mut extra_a) => result.map_err(|_| "Friends socket closed")?,
                result = drain(&mut extra_b) => result.map_err(|_| "Notification socket closed")?,
                _ = heartbeat.tick() => {
                    send_json(&mut chat_socket, ping.clone()).await?;
                    for (socket, ping) in [&mut extra_a, &mut extra_b].into_iter().flatten() {
                        send_json(socket, ping.clone()).await?;
                    }
                }
            }
            if receiver.lock().unwrap().failed.is_some() {
                return Err("Chat socket revoked or failed");
            }
        }
    }
    .await;
    if let Err(error) = result {
        receiver.lock().unwrap().failed.get_or_insert(error);
    }
}

async fn send_message(
    config: &Config,
    client: &reqwest::Client,
    sequence: usize,
    sends: &Mutex<Sends>,
) {
    let sender = sequence % config.users;
    let sent_us = config.epoch.elapsed().as_micros() as u64;
    let body = serde_json::to_string(&ChatRequest::Send {
        guild_id: GuildId::from_uuid(common::id("guild", 0)),
        channel_id: config.channel(sender),
        client_id: ClientMessageId::from_uuid(uuid::Uuid::new_v4()),
        content: format!("{PREFIX} {} {sequence} {sent_us}", config.run),
        attachment_ids: vec![],
        reply_to: None,
    })
    .unwrap();
    let mut url = config.http.clone();
    url.set_path(chat::CHAT_PATH);
    let started = Instant::now();
    let result = client
        .post(url)
        .bearer_auth(common::token(&config.secret, sender))
        .header("content-type", "application/json")
        .body(body)
        .timeout(Duration::from_secs(10))
        .send()
        .await;
    let elapsed = started.elapsed().as_millis().min(10_001) as usize;
    let mut sends = sends.lock().unwrap();
    sends.histogram[elapsed] += 1;
    match result {
        Ok(response) if response.status().is_success() => {
            sends.ok.insert(sequence);
        }
        Ok(response) => {
            *sends
                .errors
                .entry(response.status().as_u16().to_string())
                .or_default() += 1
        }
        Err(error) => {
            *sends
                .errors
                .entry(
                    if error.is_timeout() {
                        "timeout"
                    } else {
                        "transport"
                    }
                    .into(),
                )
                .or_default() += 1
        }
    }
}

fn report(
    config: &Config,
    receivers: &[Arc<Mutex<Receiver>>],
    sends: &Sends,
    connected: u64,
    complete: bool,
) -> (serde_json::Value, bool) {
    let receivers: Vec<_> = receivers.iter().map(|r| r.lock().unwrap()).collect();
    let mut histogram = vec![0; 10_002];
    let (mut expected, mut missing, mut late, mut duplicates, mut invalid) = (0, 0, 0, 0, 0);
    let mut worst_missing = 0.0_f64;
    let failures = receivers.iter().filter(|r| r.failed.is_some()).count();
    for (index, r) in receivers.iter().enumerate() {
        // Only messages posted to this receiver's channel; senders receive their own.
        let mut own = Bits::new(config.messages());
        for sequence in 0..config.messages() {
            if config.channel(sequence % config.users) == config.channel(index)
                && sends.ok.contains(sequence)
            {
                own.insert(sequence);
            }
        }
        let wanted = own.count();
        let lost = own.missing_from(&r.received);
        expected += wanted;
        missing += lost;
        late += r.late;
        duplicates += r.duplicates;
        invalid += r.invalid;
        if wanted > 0 {
            worst_missing = worst_missing.max(lost as f64 * 100.0 / wanted as f64);
        }
        for (total, count) in histogram.iter_mut().zip(&r.histogram) {
            *total += count;
        }
    }
    let send_errors: u64 = sends.errors.values().sum();
    let scheduled = config.messages() as u64;
    let passed = complete
        && failures == 0
        && connected == config.users as u64
        && send_errors == 0
        && sends.attempted == scheduled
        && invalid == 0
        && (scheduled == 0 || expected > 0)
        && (missing + late) * 1000 <= expected;
    let p = |h: &[u64], n| percentile(h, n);
    (
        json!({
            "format_version": 1, "complete": complete, "passed": passed,
            "users": config.users, "text_channels": config.channels, "full_client": config.full_client,
            "sockets_per_user": if config.full_client { 3 } else { 1 },
            "rate_messages_per_second": config.rate, "duration_seconds": config.seconds,
            "late_threshold_ms": config.late_ms,
            "connected_users": connected, "failed_users": failures,
            "scheduled_messages": scheduled, "attempted_messages": sends.attempted,
            "accepted_messages": sends.ok.count(), "send_errors": sends.errors,
            "send_p50_ms": p(&sends.histogram, 50), "send_p95_ms": p(&sends.histogram, 95), "send_p99_ms": p(&sends.histogram, 99),
            "expected_deliveries": expected, "missing": missing, "late": late,
            "duplicates": duplicates, "invalid": invalid,
            "missing_percent": if expected == 0 { None } else { Some(missing as f64 * 100.0 / expected as f64) },
            "worst_receiver_missing_percent": worst_missing,
            "delivery_p50_ms": p(&histogram, 50), "delivery_p95_ms": p(&histogram, 95), "delivery_p99_ms": p(&histogram, 99),
            "socket_events": receivers.iter().map(|r| r.events).sum::<u64>(),
            "latency_note": "HTTP send start to socket receipt on one monotonic clock: includes the send request, commit, wake-up, per-socket authorization/read and both network legs. 10001 means >=10001ms.",
        }),
        passed,
    )
}

async fn run() -> Result<bool> {
    let mut args = Args::parse()?;
    if args.flag("--help") {
        println!(
            "chat_load --url http://127.0.0.1:3001 --allow-insecure --users 64 --rate 4 --seconds 60\nOptions: --users 2..500 (64), --text-channels 1..16 (1, must match the server),\n--rate 0..250 messages/s across all users (4; at most users/2 for the 30/min account limit),\n--seconds 1..3600 (60), --late-ms 1..10000 (1000), --chat-only (skip friends/notification sockets),\n--output report.json. Requires THISCORD_LOAD_SECRET matching voice_load_server. See docs/chat-load.md."
        );
        return Ok(true);
    }
    let url = args.text("--url", "http://127.0.0.1:3001");
    let users = args.number("--users", 64, 2, 500)?;
    let channels = args.number("--text-channels", 1, 1, 16)?;
    let rate: f64 = args
        .text("--rate", "4")
        .parse()
        .ok()
        .filter(|r: &f64| (0.0..=250.0).contains(r))
        .ok_or("--rate must be 0..250 messages per second")?;
    if rate * 60.0 > users as f64 * MAX_SENDS_PER_ACCOUNT_PER_MINUTE {
        return Err("--rate exceeds the 30 messages/minute account limit; add users or lower it");
    }
    let seconds = args.number("--seconds", 60, 1, 3600)?;
    let late_ms = args.number("--late-ms", 1000, 1, 10_000)? as u64;
    let full_client = !args.flag("--chat-only");
    let insecure = args.flag("--allow-insecure");
    let output = args.text("--output", "");
    args.finish()?;
    let mut output = if output.is_empty() {
        None
    } else {
        Some(
            std::fs::OpenOptions::new()
                .create_new(true)
                .write(true)
                .open(output)
                .map_err(|_| "Cannot create output file (must not already exist)")?,
        )
    };
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (http, ws) = origins(&url, insecure)?;
    let config = Arc::new(Config {
        http,
        ws,
        secret: common::secret()?,
        users,
        channels,
        rate,
        seconds,
        late_ms,
        full_client,
        epoch: Instant::now(),
        run: uuid::Uuid::new_v4().simple().to_string()[..12].to_owned(),
    });
    let receivers: Vec<_> = (0..users)
        .map(|_| Arc::new(Mutex::new(Receiver::new(config.messages()))))
        .collect();
    let connected = Arc::new(AtomicU64::new(0));
    let (stop_tx, stop_rx) = watch::channel(false);
    let mut tasks = tokio::task::JoinSet::new();
    println!(
        "Connecting {users} users ({} socket(s) each) to {channels} text channel(s)...",
        if full_client { 3 } else { 1 }
    );
    let joins = Arc::new(tokio::sync::Semaphore::new(16));
    for (index, receiver) in receivers.iter().enumerate() {
        let (config, receiver, connected, stop, joins) = (
            config.clone(),
            receiver.clone(),
            connected.clone(),
            stop_rx.clone(),
            joins.clone(),
        );
        tasks.spawn(async move {
            // At most 16 users connect at once; each releases its slot when connected.
            let Ok(slot) = joins.acquire_owned().await else {
                return;
            };
            user(config, index, receiver, connected, stop, slot).await;
        });
    }
    let started = Instant::now();
    while connected.load(Relaxed) < users as u64
        && started.elapsed() < Duration::from_secs(120)
        && receivers.iter().all(|r| r.lock().unwrap().failed.is_none())
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let all = connected.load(Relaxed);
    println!(
        "Connected {all}/{users} users in {:.1}s",
        started.elapsed().as_secs_f64()
    );
    let sends = Arc::new(Mutex::new(Sends {
        ok: Bits::new(config.messages()),
        histogram: vec![0; 10_002],
        ..Default::default()
    }));
    let complete = if all == users as u64 {
        // Let subscription snapshots, rosters and member lists settle first.
        tokio::time::sleep(Duration::from_secs(3)).await;
        println!(
            "Measuring {seconds}s at {rate} messages/s ({} messages), then draining 5s.",
            config.messages()
        );
        let client = reqwest::Client::builder()
            .pool_max_idle_per_host(64)
            .build()
            .map_err(|_| "HTTP client setup failed")?;
        let begin = tokio::time::Instant::now();
        let mut sending = tokio::task::JoinSet::new();
        for sequence in 0..config.messages() {
            let at = begin + Duration::from_secs_f64(sequence as f64 / rate);
            tokio::time::sleep_until(at).await;
            sends.lock().unwrap().attempted += 1;
            let (config, client, sends) = (config.clone(), client.clone(), sends.clone());
            sending.spawn(async move { send_message(&config, &client, sequence, &sends).await });
        }
        tokio::time::sleep_until(begin + Duration::from_secs(seconds as u64)).await;
        while sending.join_next().await.is_some() {}
        tokio::time::sleep(Duration::from_secs(5)).await;
        true
    } else {
        eprintln!("Not every user connected; see failures in the report.");
        false
    };
    let _ = stop_tx.send(true);
    let _ = tokio::time::timeout(Duration::from_secs(5), async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    let (report, passed) = report(
        &config,
        &receivers,
        &sends.lock().unwrap(),
        connected.load(Relaxed),
        complete,
    );
    println!(
        "{}: accepted={}/{}, send errors={}, deliveries expected={}, missing={}, late={}, delivery p50={}ms p99={}ms, send p99={}ms",
        if passed { "PASS" } else { "FAIL/INCOMPLETE" },
        report["accepted_messages"],
        report["scheduled_messages"],
        report["send_errors"],
        report["expected_deliveries"],
        report["missing"],
        report["late"],
        report["delivery_p50_ms"],
        report["delivery_p99_ms"],
        report["send_p99_ms"]
    );
    if let Some(file) = &mut output {
        serde_json::to_writer_pretty(file, &report).map_err(|_| "Cannot write report")?;
    }
    Ok(passed)
}

#[tokio::main]
async fn main() {
    match run().await {
        Ok(true) => {}
        Ok(false) => std::process::exit(2),
        Err(e) => {
            eprintln!("FAIL: {e}");
            std::process::exit(1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> Config {
        Config {
            http: url::Url::parse("http://localhost").unwrap(),
            ws: url::Url::parse("ws://localhost").unwrap(),
            secret: String::new(),
            users: 4,
            channels: 2,
            rate: 2.0,
            seconds: 2,
            late_ms: 100,
            full_client: false,
            epoch: Instant::now(),
            run: "abc".into(),
        }
    }

    fn message(content: &str) -> String {
        serde_json::json!({"version": 1, "event": {"type": "update", "subscription": 1, "event": {"type": "message", "cursor": 1, "message": {
            "id": uuid::Uuid::nil(), "client_id": uuid::Uuid::nil(), "guild_id": uuid::Uuid::nil(), "channel_id": uuid::Uuid::nil(),
            "author_id": null, "username": "u", "content": content, "mentions": [], "created_at": "2026-01-01T00:00:00Z",
            "edited_at": null, "deleted": false, "revision": 1, "sequence": 1}}}})
        .to_string()
    }

    #[test]
    fn origins_reject_credentials_and_unapproved_cleartext() {
        assert!(origins("http://localhost:3001", false).is_err());
        assert!(origins("https://user:pw@example.com", false).is_err());
        assert!(origins("https://example.com/?t=1", false).is_err());
        assert_eq!(
            origins("https://example.com", false).unwrap().1.as_str(),
            "wss://example.com/"
        );
    }

    #[test]
    fn records_only_this_runs_messages_once_and_reports_channel_scoped_loss() {
        let c = config();
        let r = Mutex::new(Receiver::new(c.messages()));
        let now = c.epoch.elapsed().as_micros() as u64;
        record(&c, &r, &message(&format!("chatload abc 0 {now}")));
        record(&c, &r, &message(&format!("chatload abc 0 {now}")));
        record(&c, &r, &message(&format!("chatload old 1 {now}")));
        record(&c, &r, &message(&format!("chatload abc 99 {now}")));
        {
            let r = r.lock().unwrap();
            assert_eq!(r.received.count(), 1);
            assert_eq!(r.duplicates, 1);
            assert_eq!(r.invalid, 2);
            assert!(r.failed.is_none());
        }
        // Messages 0 and 2 go to channel 0 (users 0 and 2), 1 and 3 to channel 1.
        let mut sends = Sends {
            ok: Bits::new(4),
            attempted: 4,
            histogram: vec![0; 10_002],
            ..Default::default()
        };
        for n in 0..4 {
            sends.ok.insert(n);
        }
        let receivers: Vec<_> = (0..4)
            .map(|_| Arc::new(Mutex::new(Receiver::new(4))))
            .collect();
        let (report, passed) = report(&c, &receivers, &sends, 4, true);
        assert!(!passed);
        assert_eq!(report["expected_deliveries"], 8);
        assert_eq!(report["missing"], 8);
        for (index, receiver) in receivers.iter().enumerate() {
            for n in (index % 2..4).step_by(2) {
                receiver.lock().unwrap().received.insert(n);
            }
        }
        assert!(super::report(&c, &receivers, &sends, 4, true).1);
        receivers[0].lock().unwrap().failed = Some("revoked");
        assert!(!super::report(&c, &receivers, &sends, 4, true).1);
    }
}
