//! System audio without Thiscord's own voice playback. A private PipeWire
//! capture stream (never auto-connected, so it is not a device and cannot
//! become a default) receives extra links from every other application's
//! output ports; their existing links to speakers are untouched. Nodes whose
//! client or application process is this process or one of its descendants
//! (WebKitWebProcess, helpers) are never linked.
//!
//! The PipeWire realtime callback only converts to f32 and pushes whole
//! buffers into a bounded SPSC ring: no codec, locks or allocation. Opus and
//! RTP run on the caller's thread via `sink`.
use pipewire as pw;
use ringbuf::{
    HeapRb,
    traits::{Consumer, Observer, Producer, Split},
};
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

use super::super::metrics::Metrics;

const RATE: u32 = 48_000;
/// 500 ms of interleaved stereo; overflow drops whole buffers.
const RING_SAMPLES: usize = RATE as usize;
/// Idle bound for the consumer: the realtime callback unparks it after each
/// committed buffer, so this only paces stop/failure checks while silent
/// (20 wakes/s instead of a 5 ms poll's 200).
const IDLE_WAIT: Duration = Duration::from_millis(50);
const UNAVAILABLE: &str = "System audio needs a running PipeWire session (pipewire and wireplumber). Video-only sharing is still available.";

#[derive(Default)]
struct Counters {
    overflow: AtomicU64,
    samples: AtomicU64,
    links: AtomicU32,
    failed: AtomicBool,
}

/// Blocks until `stop`, `!running()` or failure, delivering interleaved
/// 48 kHz stereo f32 to `sink` on this thread.
pub fn system_audio(
    stop: &AtomicBool,
    running: &dyn Fn() -> bool,
    metrics: &Metrics,
    sink: &mut dyn FnMut(&[f32]) -> Result<(), String>,
) -> Result<(), String> {
    let (producer, mut consumer) = HeapRb::<f32>::new(RING_SAMPLES).split();
    let quit = Arc::new(AtomicBool::new(false));
    let counters = Arc::new(Counters::default());
    let (ready_tx, ready_rx) = mpsc::channel();
    let consumer_thread = std::thread::current();
    let graph = {
        let quit = quit.clone();
        let counters = counters.clone();
        std::thread::Builder::new()
            .name("thiscord-system-audio-pw".into())
            .spawn(move || {
                let result = run_graph(producer, consumer_thread, &quit, &counters, &ready_tx);
                if let Err(error) = result {
                    counters.failed.store(true, Ordering::Release);
                    let _ = ready_tx.send(Err(error));
                }
            })
            .map_err(|_| "Cannot start system audio capture.")?
    };
    let finish = |result: Result<(), String>| {
        quit.store(true, Ordering::Release);
        let _ = graph.join();
        result
    };
    match ready_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(error)) => return finish(Err(error)),
        Err(_) => return finish(Err(UNAVAILABLE.into())),
    }
    metrics.label("audio_capture_backend", "pipewire_private_stream");
    let mut buffer = vec![0.0_f32; 4800];
    let (mut overflow, mut samples, mut links) = (0, 0, u32::MAX);
    let result = (|| {
        while !stop.load(Ordering::Acquire) && running() {
            metrics.add("audio_capture_wakes", 1);
            if counters.failed.load(Ordering::Acquire) {
                return Err("PipeWire system audio capture was interrupted.".into());
            }
            // Whole stereo frames only; the producer commits whole buffers.
            let available = consumer.occupied_len().min(buffer.len()) & !1;
            if available == 0 {
                std::thread::park_timeout(IDLE_WAIT);
            } else {
                let n = consumer.pop_slice(&mut buffer[..available]);
                sink(&buffer[..n & !1])?;
            }
            let now = counters.overflow.load(Ordering::Relaxed);
            if now != overflow {
                metrics.add("audio_capture_queue_resets", now - overflow);
                overflow = now;
            }
            let now = counters.samples.load(Ordering::Relaxed);
            if now != samples {
                metrics.add("audio_capture_packets", 1);
                samples = now;
            }
            let now = counters.links.load(Ordering::Relaxed);
            if now != links {
                metrics.label("audio_capture_links", now);
                links = now;
            }
        }
        Ok(())
    })();
    finish(result)
}

struct Callback {
    producer: ringbuf::HeapProd<f32>,
    /// The consumer; `unpark` is a futex wake (no lock or allocation).
    consumer: std::thread::Thread,
    counters: Arc<Counters>,
}

/// Owns the PipeWire main loop for the duration of the capture.
fn run_graph(
    producer: ringbuf::HeapProd<f32>,
    consumer: std::thread::Thread,
    quit: &Arc<AtomicBool>,
    counters: &Arc<Counters>,
    ready: &mpsc::Sender<Result<(), String>>,
) -> Result<(), String> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(|_| UNAVAILABLE)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(|_| UNAVAILABLE)?;
    let core = context.connect_rc(None).map_err(|_| UNAVAILABLE)?;
    let registry = core.get_registry_rc().map_err(|_| UNAVAILABLE)?;
    let own_pid = std::process::id();
    let node_name = format!("thiscord-system-audio-{own_pid}");

    let failed = counters.clone();
    let weak = mainloop.downgrade();
    let _core_listener = core
        .add_listener_local()
        .error(move |id, _, _, _| {
            if id == pw::core::PW_ID_CORE {
                failed.failed.store(true, Ordering::Release);
                if let Some(mainloop) = weak.upgrade() {
                    mainloop.quit();
                }
            }
        })
        .register();

    let stream = pw::stream::StreamRc::new(
        core.clone(),
        "thiscord-system-audio",
        pw::properties::properties! {
            "media.type" => "Audio",
            "media.category" => "Capture",
            "media.role" => "Production",
            "node.name" => node_name.as_str(),
            "node.description" => "Thiscord shared audio",
            // Private: the session manager must not link or select it.
            "node.autoconnect" => "false",
            "node.dont-reconnect" => "true",
            "node.want-driver" => "true",
            "audio.rate" => "48000",
            "audio.channels" => "2",
            "audio.position" => "[ FL FR ]",
        },
    )
    .map_err(|_| UNAVAILABLE)?;
    let _stream_listener = stream
        .add_local_listener_with_user_data(Callback {
            producer,
            consumer,
            counters: counters.clone(),
        })
        .process(|stream, callback| {
            let Some(mut buffer) = stream.dequeue_buffer() else {
                return;
            };
            let Some(data) = buffer.datas_mut().first_mut() else {
                return;
            };
            let (offset, size) = (data.chunk().offset() as usize, data.chunk().size() as usize);
            let Some(bytes) = data.data() else {
                return;
            };
            let Some(bytes) = bytes.get(offset..offset.saturating_add(size)) else {
                return;
            };
            let samples = (bytes.len() / 4) & !1;
            if callback.producer.vacant_len() < samples {
                callback.counters.overflow.fetch_add(1, Ordering::Relaxed);
                return;
            }
            // Stack staging keeps the callback allocation-free; chunks are even.
            let mut staging = [0.0_f32; 512];
            for chunk in bytes[..samples * 4].chunks(staging.len() * 4) {
                let n = chunk.len() / 4;
                for (out, raw) in staging.iter_mut().zip(chunk.as_chunks::<4>().0) {
                    let value = f32::from_le_bytes(*raw);
                    *out = if value.is_finite() { value } else { 0.0 };
                }
                callback.producer.push_slice(&staging[..n]);
            }
            callback.consumer.unpark();
            callback
                .counters
                .samples
                .fetch_add(samples as u64, Ordering::Relaxed);
        })
        .register()
        .map_err(|_| UNAVAILABLE)?;

    let mut info = pw::spa::param::audio::AudioInfoRaw::new();
    info.set_format(pw::spa::param::audio::AudioFormat::F32LE);
    info.set_rate(RATE);
    info.set_channels(2);
    let mut position = [0; pw::spa::sys::SPA_AUDIO_MAX_CHANNELS as usize];
    position[0] = pw::spa::sys::SPA_AUDIO_CHANNEL_FL;
    position[1] = pw::spa::sys::SPA_AUDIO_CHANNEL_FR;
    info.set_position(position);
    let format = pw::spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &pw::spa::pod::Value::Object(pw::spa::pod::Object {
            type_: pw::spa::utils::SpaTypes::ObjectParamFormat.as_raw(),
            id: pw::spa::param::ParamType::EnumFormat.as_raw(),
            properties: info.into(),
        }),
    )
    .map_err(|_| UNAVAILABLE)?
    .0
    .into_inner();
    let mut params = [pw::spa::pod::Pod::from_bytes(&format).ok_or(UNAVAILABLE)?];
    stream
        .connect(
            pw::spa::utils::Direction::Input,
            None,
            pw::stream::StreamFlags::MAP_BUFFERS | pw::stream::StreamFlags::RT_PROCESS,
            &mut params,
        )
        .map_err(|_| UNAVAILABLE)?;

    let graph = Rc::new(RefCell::new(Graph::new(own_pid, node_name)));
    let added = {
        let graph = graph.clone();
        let core = core.clone();
        let counters = counters.clone();
        move |global: &pw::registry::GlobalObject<&pw::spa::utils::dict::DictRef>| {
            let Some(props) = global.props else {
                return;
            };
            let mut graph = graph.borrow_mut();
            let get = |key: &str| props.get(key);
            match global.type_ {
                pw::types::ObjectType::Client => graph.client(global.id, get),
                pw::types::ObjectType::Node => graph.node(global.id, get),
                pw::types::ObjectType::Port => graph.port(global.id, get),
                _ => return,
            }
            graph.reconcile(&core);
            counters
                .links
                .store(graph.links.len() as u32, Ordering::Relaxed);
        }
    };
    let removed = {
        let graph = graph.clone();
        let counters = counters.clone();
        move |id| {
            let mut graph = graph.borrow_mut();
            graph.remove(id);
            counters
                .links
                .store(graph.links.len() as u32, Ordering::Relaxed);
        }
    };
    let _registry_listener = registry
        .add_listener_local()
        .global(added)
        .global_remove(removed)
        .register();

    let quit = quit.clone();
    let weak = mainloop.downgrade();
    let timer = mainloop.loop_().add_timer(move |_| {
        if quit.load(Ordering::Acquire)
            && let Some(mainloop) = weak.upgrade()
        {
            mainloop.quit();
        }
    });
    let _ = timer.update_timer(
        Some(Duration::from_millis(20)),
        Some(Duration::from_millis(50)),
    );
    let _ = ready.send(Ok(()));
    mainloop.run();
    // Links and the stream are owned by this client; dropping them (and the
    // connection) removes them from the graph.
    graph.borrow_mut().links.clear();
    let _ = stream.disconnect();
    Ok(())
}

struct PortInfo {
    node: u32,
    output: bool,
    channel: String,
}
/// Registry mirror used to decide which application ports to link.
struct Graph {
    own_pid: u32,
    node_name: String,
    own_node: Option<u32>,
    clients: HashMap<u32, Option<u32>>,
    /// Stream/Output/Audio nodes: (client id, application pid).
    sources: HashMap<u32, (Option<u32>, Option<u32>)>,
    ports: HashMap<u32, PortInfo>,
    links: HashMap<(u32, u32), pw::link::Link>,
    failed: HashSet<(u32, u32)>,
}
impl Graph {
    fn new(own_pid: u32, node_name: String) -> Self {
        Self {
            own_pid,
            node_name,
            own_node: None,
            clients: HashMap::new(),
            sources: HashMap::new(),
            ports: HashMap::new(),
            links: HashMap::new(),
            failed: HashSet::new(),
        }
    }
    fn client<'a>(&mut self, id: u32, get: impl Fn(&str) -> Option<&'a str>) {
        // Trusted, server-side peer credentials.
        let pid = get("pipewire.sec.pid").and_then(|p| p.parse().ok());
        self.clients.insert(id, pid);
    }
    fn node<'a>(&mut self, id: u32, get: impl Fn(&str) -> Option<&'a str>) {
        if get("node.name") == Some(self.node_name.as_str()) {
            self.own_node = Some(id);
            return;
        }
        if get("media.class") == Some("Stream/Output/Audio") {
            let client = get("client.id").and_then(|c| c.parse().ok());
            let pid = get("application.process.id").and_then(|p| p.parse().ok());
            self.sources.insert(id, (client, pid));
        }
    }
    fn port<'a>(&mut self, id: u32, get: impl Fn(&str) -> Option<&'a str>) {
        if get("port.monitor") == Some("true") {
            return;
        }
        let (Some(node), Some(direction)) = (
            get("node.id").and_then(|n| n.parse().ok()),
            get("port.direction"),
        ) else {
            return;
        };
        self.ports.insert(
            id,
            PortInfo {
                node,
                output: direction == "out",
                channel: get("audio.channel").unwrap_or("MONO").to_owned(),
            },
        );
    }
    fn remove(&mut self, id: u32) {
        self.clients.remove(&id);
        self.sources.remove(&id);
        self.ports.remove(&id);
        if self.own_node == Some(id) {
            self.own_node = None;
        }
        self.links
            .retain(|(out, input), _| *out != id && *input != id);
        self.failed
            .retain(|(out, input)| *out != id && *input != id);
    }
    fn excluded(&self, node: u32) -> bool {
        let Some(&(client, pid)) = self.sources.get(&node) else {
            return true;
        };
        let client_pid = client.and_then(|c| self.clients.get(&c).copied().flatten());
        [client_pid, pid]
            .into_iter()
            .flatten()
            .any(|pid| in_process_tree(pid, self.own_pid, parent_of))
    }
    /// (output port, input port) pairs that should exist now.
    fn wanted(&self) -> Vec<(u32, u32, u32, u32)> {
        let Some(own) = self.own_node else {
            return Vec::new();
        };
        let inputs: Vec<_> = self
            .ports
            .iter()
            .filter(|(_, p)| p.node == own && !p.output)
            .collect();
        let mut wanted = Vec::new();
        for (&out_id, out) in self.ports.iter().filter(|(_, p)| p.output) {
            if !self.sources.contains_key(&out.node) || self.excluded(out.node) {
                continue;
            }
            for (in_id, input) in &inputs {
                if channel_matches(&out.channel, &input.channel) {
                    wanted.push((out.node, out_id, own, **in_id));
                }
            }
        }
        wanted
    }
    fn reconcile(&mut self, core: &pw::core::CoreRc) {
        for (out_node, out_port, in_node, in_port) in self.wanted() {
            let key = (out_port, in_port);
            if self.links.contains_key(&key) || self.failed.contains(&key) {
                continue;
            }
            let created = core.create_object::<pw::link::Link>(
                "link-factory",
                &pw::properties::properties! {
                    "link.output.node" => out_node.to_string(),
                    "link.output.port" => out_port.to_string(),
                    "link.input.node" => in_node.to_string(),
                    "link.input.port" => in_port.to_string(),
                    "object.linger" => "false",
                },
            );
            match created {
                Ok(link) => {
                    self.links.insert(key, link);
                }
                Err(_) => {
                    self.failed.insert(key);
                }
            }
        }
    }
}

/// Stereo capture: mono sources feed both sides; FL/FR map by name.
fn channel_matches(output: &str, input: &str) -> bool {
    match output {
        "MONO" => true,
        "FL" | "FR" => output == input,
        // Front-centre/LFE and surround fold into nothing; streams downmix
        // themselves when their output is configured as stereo.
        _ => false,
    }
}

fn parent_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // The command name can contain spaces/parentheses; fields follow the last ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    rest.split_whitespace().nth(1)?.parse().ok()
}
/// True when `pid` is `root` or one of its descendants (bounded walk).
fn in_process_tree(mut pid: u32, root: u32, parent: impl Fn(u32) -> Option<u32>) -> bool {
    for _ in 0..64 {
        if pid == root {
            return true;
        }
        match parent(pid) {
            Some(next) if next > 1 && next != pid => pid = next,
            _ => return false,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn process_tree_excludes_self_and_descendants_only() {
        let parents: HashMap<u32, u32> = [(500, 100), (600, 500), (700, 1), (800, 700)].into();
        let parent = |pid| parents.get(&pid).copied();
        assert!(in_process_tree(100, 100, parent));
        assert!(in_process_tree(500, 100, parent));
        assert!(
            in_process_tree(600, 100, parent),
            "grandchild (WebKitWebProcess)"
        );
        assert!(!in_process_tree(800, 100, parent));
        assert!(!in_process_tree(700, 100, parent));
        assert!(
            !in_process_tree(42, 100, parent),
            "unknown pids are other apps"
        );
        // The real /proc walk recognises this test process and its parent chain.
        let me = std::process::id();
        assert!(in_process_tree(me, me, parent_of));
        if let Some(parent) = parent_of(me) {
            assert!(!in_process_tree(parent, me, parent_of));
        }
    }

    #[test]
    fn stereo_capture_maps_channels() {
        assert!(channel_matches("MONO", "FL") && channel_matches("MONO", "FR"));
        assert!(channel_matches("FL", "FL") && !channel_matches("FL", "FR"));
        assert!(!channel_matches("FC", "FL") && !channel_matches("LFE", "FR"));
    }

    #[test]
    fn graph_links_other_applications_but_never_this_process_tree() {
        let me = std::process::id();
        let mut graph = Graph::new(me, "thiscord-system-audio-1".into());
        let props = |pairs: &'static [(&'static str, &'static str)]| {
            move |key: &str| pairs.iter().find(|pair| pair.0 == key).map(|pair| pair.1)
        };
        graph.node(10, props(&[("node.name", "thiscord-system-audio-1")]));
        graph.port(
            11,
            props(&[
                ("node.id", "10"),
                ("port.direction", "in"),
                ("audio.channel", "FL"),
            ]),
        );
        graph.port(
            12,
            props(&[
                ("node.id", "10"),
                ("port.direction", "in"),
                ("audio.channel", "FR"),
            ]),
        );
        // Another application, via pipewire-pulse (client pid is the server).
        graph.client(20, props(&[("pipewire.sec.pid", "1")]));
        graph.node(
            21,
            props(&[
                ("media.class", "Stream/Output/Audio"),
                ("client.id", "20"),
                ("application.process.id", "999999"),
            ]),
        );
        graph.port(
            22,
            props(&[
                ("node.id", "21"),
                ("port.direction", "out"),
                ("audio.channel", "FL"),
            ]),
        );
        graph.port(
            23,
            props(&[
                ("node.id", "21"),
                ("port.direction", "out"),
                ("audio.channel", "FR"),
            ]),
        );
        graph.port(
            24,
            props(&[
                ("node.id", "21"),
                ("port.direction", "out"),
                ("audio.channel", "FL"),
                ("port.monitor", "true"),
            ]),
        );
        // This process's own voice playback, identified by trusted credentials.
        graph.clients.insert(30, Some(me));
        graph.sources.insert(31, (Some(30), None));
        graph.ports.insert(
            32,
            PortInfo {
                node: 31,
                output: true,
                channel: "FL".into(),
            },
        );
        let mut wanted = graph.wanted();
        wanted.sort();
        assert_eq!(wanted, vec![(21, 22, 10, 11), (21, 23, 10, 12)]);
        graph.remove(21);
        assert!(graph.wanted().is_empty());
    }

    /// Needs a PipeWire session with another application playing (see
    /// docs). Run with `--ignored` inside a desktop session.
    #[test]
    #[ignore = "requires a running PipeWire session with an application playing"]
    fn captures_other_applications_from_a_live_pipewire_session() {
        let stop = AtomicBool::new(false);
        let metrics = Metrics::default();
        let started = std::time::Instant::now();
        let (mut total, mut peak) = (0, 0.0_f32);
        system_audio(
            &stop,
            &|| started.elapsed() < Duration::from_secs(2),
            &metrics,
            &mut |samples| {
                assert_eq!(samples.len() % 2, 0);
                total += samples.len();
                peak = samples.iter().fold(peak, |p, s| p.max(s.abs()));
                Ok(())
            },
        )
        .unwrap();
        let snapshot = metrics.snapshot();
        let wakes = snapshot.counters["audio_capture_wakes"];
        eprintln!(
            "captured {total} samples, peak {peak}, links {:?}, consumer wakes {wakes} in 2 s",
            snapshot.labels.get("audio_capture_links")
        );
        // Woken per committed PipeWire buffer (about 47/s at a 1024 quantum),
        // not by a 5 ms poll (400 in 2 s).
        assert!(wakes < 250, "consumer should wake per buffer, not poll");
        assert!(total > 48_000, "expected over 0.5 s of stereo audio");
        assert!(peak > 0.05, "captured audio should not be silent");
    }
}
