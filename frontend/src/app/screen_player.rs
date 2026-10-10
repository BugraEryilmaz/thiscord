//! Browser-native H.264 decoding/compositing. Rust/WASM only controls signaling.
use crate::account_client::native;
use leptos::prelude::*;
use serde_json::json;
use thiscord_shared::screen::{ViewAnswer, Watch};
use wasm_bindgen::{JsCast, prelude::*};
use wasm_bindgen_futures::JsFuture;
use web_sys::*;

struct Player {
    pc: RtcPeerConnection,
    video: HtmlVideoElement,
    _track: Closure<dyn FnMut(RtcTrackEvent)>,
}
impl Drop for Player {
    fn drop(&mut self) {
        self.pc.set_ontrack(None);
        self.pc.close();
        self.video.set_src_object(None);
    }
}
fn visible() -> bool {
    web_sys::window()
        .and_then(|w| w.document())
        .is_some_and(|d| !d.hidden())
}
async fn connect(video: HtmlVideoElement, watch: Watch) -> Result<Player, String> {
    let pc = RtcPeerConnection::new().map_err(|_| "This WebView cannot play WebRTC video")?;
    let target = video.clone();
    let track = Closure::<dyn FnMut(RtcTrackEvent)>::new(move |event: RtcTrackEvent| {
        if let Ok(stream) = MediaStream::new() {
            stream.add_track(&event.track());
            target.set_muted(true);
            target.set_src_object(Some(&stream));
            let _ = target.play();
        }
    });
    pc.set_ontrack(Some(track.as_ref().unchecked_ref()));
    let player = Player {
        pc,
        video,
        _track: track,
    };
    let init = RtcRtpTransceiverInit::new();
    init.set_direction(RtcRtpTransceiverDirection::Recvonly);
    player.pc.add_transceiver_with_str_and_init("video", &init);
    let offer = JsFuture::from(player.pc.create_offer())
        .await
        .map_err(|_| "Cannot create video player")?;
    let offer: RtcSessionDescriptionInit = offer.unchecked_into();
    JsFuture::from(player.pc.set_local_description(&offer))
        .await
        .map_err(|_| "Cannot initialize video player")?;
    // Only connection setup uses a timer; frames flow through WebRTC directly.
    for _ in 0..100 {
        if player.pc.ice_gathering_state() == RtcIceGatheringState::Complete {
            break;
        }
        gloo_timers::future::TimeoutFuture::new(50).await;
    }
    if player.pc.ice_gathering_state() != RtcIceGatheringState::Complete {
        return Err("Video player connection timed out".into());
    }
    let sdp = player
        .pc
        .local_description()
        .ok_or("Missing video offer")?
        .sdp();
    let answer: ViewAnswer = native(
        "screen_view_open",
        json!({"offer":{"watch":watch,"sdp":sdp}}),
    )
    .await?;
    let remote = RtcSessionDescriptionInit::new(RtcSdpType::Answer);
    remote.set_sdp(&answer.sdp);
    JsFuture::from(player.pc.set_remote_description(&remote))
        .await
        .map_err(|_| "Cannot connect video player")?;
    Ok(player)
}

pub async fn run(
    video: HtmlVideoElement,
    watch: Watch,
    message: RwSignal<String>,
    diagnostics: RwSignal<String>,
) {
    loop {
        while !visible() {
            gloo_timers::future::TimeoutFuture::new(250).await;
        }
        match connect(video.clone(), watch).await {
            Ok(player) => {
                let mut pending = 0;
                let mut previous = thiscord_shared::screen::Diagnostics::default();
                let mut last_stats = 0.0;
                let mut last_presented = 0;
                let mut last_presentation = js_sys::Date::now();
                let mut last_pli = 0;
                let mut last_dropped = 0;
                let mut decoder_pressure = false;
                while visible() {
                    let state = player.pc.connection_state();
                    if matches!(
                        state,
                        RtcPeerConnectionState::Closed | RtcPeerConnectionState::Failed
                    ) {
                        break;
                    }
                    if state == RtcPeerConnectionState::Connected {
                        message.set(String::new());
                        pending = 0;
                    } else {
                        pending += 1;
                        if pending > 40 {
                            break;
                        }
                    }
                    if !native::<bool>(
                        "screen_view_keepalive",
                        json!({"watch":watch,"visible":true,"decoderPressure":decoder_pressure}),
                    )
                    .await
                    .unwrap_or(false)
                    {
                        break;
                    }
                    decoder_pressure = false;
                    let now = js_sys::Date::now();
                    if now - last_stats > 200.0 {
                        let seconds = (now - last_stats) / 1000.0;
                        last_stats = now;
                        if let Ok(stats) = native::<thiscord_shared::screen::Diagnostics>(
                            "screen_view_diagnostics",
                            json!({"watch":watch}),
                        )
                        .await
                        {
                            let (browser, pli) = browser_stats(&player.pc).await;
                            if pli > last_pli {
                                let _ =
                                    native::<bool>("screen_view_keyframe", json!({"watch":watch}))
                                        .await;
                            }
                            decoder_pressure = pli > last_pli;
                            last_pli = pli;
                            let playback = {
                                let q = player.video.get_video_playback_quality();
                                decoder_pressure |=
                                    q.dropped_video_frames().saturating_sub(last_dropped) > 0;
                                last_dropped = q.dropped_video_frames();
                                let presented = q
                                    .total_video_frames()
                                    .saturating_sub(q.dropped_video_frames());
                                if presented > last_presented {
                                    last_presentation = now;
                                } else {
                                    decoder_pressure |= now - last_presentation > 3000.0;
                                }
                                let fps = presented.saturating_sub(last_presented) as f64
                                    / seconds.max(0.001);
                                last_presented = presented;
                                format!(
                                    "Presented FPS: {fps:.1}; total={}, dropped={}\n",
                                    q.total_video_frames(),
                                    q.dropped_video_frames()
                                )
                            };
                            diagnostics.set(format!(
                                "{}\n{}{}",
                                describe(&stats, &previous),
                                playback,
                                browser
                            ));
                            previous = stats;
                        }
                    }
                    gloo_timers::future::TimeoutFuture::new(250).await;
                }
                drop(player);
            }
            Err(error) => {
                message.set(error);
            }
        }
        let _ = native::<bool>(
            "screen_view_keepalive",
            json!({"watch":watch,"visible":false}),
        )
        .await;
        gloo_timers::future::TimeoutFuture::new(1000).await;
    }
}

/// Report rates from interval deltas, not selected/nominal quality.
pub fn describe(
    current: &thiscord_shared::screen::Diagnostics,
    previous: &thiscord_shared::screen::Diagnostics,
) -> String {
    let seconds = current.elapsed_ms.saturating_sub(previous.elapsed_ms) as f64 / 1000.0;
    let mut text = format!(
        "Pipeline sample: {:.3}s since start\n",
        current.elapsed_ms as f64 / 1000.0
    );
    for (name, value) in &current.labels {
        text.push_str(&format!("{name}: {value}\n"));
    }
    for (name, value) in &current.counters {
        let rate = if seconds > 0.0 {
            value.saturating_sub(*previous.counters.get(name).unwrap_or(&0)) as f64 / seconds
        } else {
            0.0
        };
        if name.starts_with("sfu_") || name.contains("peak") {
            text.push_str(&format!("{name}: {value}\n"));
        } else {
            text.push_str(&format!("{name}: {value} ({rate:.1}/s)\n"));
        }
    }
    for (name, value) in &current.timings {
        text.push_str(&format!(
            "{name}: mean {:.2} ms, max {:.2} ms, samples {}\n",
            value.total_us as f64 / value.count.max(1) as f64 / 1000.0,
            value.max_us as f64 / 1000.0,
            value.count
        ));
    }
    for (at, event) in &current.events {
        text.push_str(&format!("[{:.3}s] {event}\n", *at as f64 / 1000.0));
    }
    text
}
async fn browser_stats(pc: &RtcPeerConnection) -> (String, u64) {
    // Whitelist scalars: SDP, candidate addresses and stream identifiers stay out.
    let future = JsFuture::from(pc.get_stats());
    let result = futures_util::future::select(
        future,
        Box::pin(gloo_timers::future::TimeoutFuture::new(100)),
    )
    .await;
    let futures_util::future::Either::Left((Ok(report), _)) = result else {
        return ("Browser stats unavailable".into(), 0);
    };
    let mut output = serde_json::Map::new();
    if let Ok(Some(entries)) = js_sys::try_iter(&report) {
        for entry in entries.flatten() {
            let entry = js_sys::Array::from(&entry).get(1);
            let get = |name: &str| {
                js_sys::Reflect::get(&entry, &JsValue::from_str(name)).unwrap_or(JsValue::UNDEFINED)
            };
            if get("type").as_string().as_deref() != Some("inbound-rtp")
                || get("kind").as_string().as_deref() != Some("video")
            {
                continue;
            }
            for name in [
                "framesPerSecond",
                "framesDecoded",
                "framesDropped",
                "framesReceived",
                "keyFramesDecoded",
                "freezeCount",
                "totalFreezesDuration",
                "packetsLost",
                "packetsReceived",
                "jitter",
                "jitterBufferDelay",
                "jitterBufferEmittedCount",
                "totalDecodeTime",
                "nackCount",
                "pliCount",
                "decoderImplementation",
                "powerEfficientDecoder",
            ] {
                let value = get(name);
                if let Some(number) = value.as_f64() {
                    output.insert(name.into(), json!(number));
                } else if let Some(value) = value.as_bool() {
                    output.insert(name.into(), json!(value));
                } else if let Some(value) = value.as_string() {
                    output.insert(name.into(), json!(value));
                }
            }
        }
    }
    let pli = output
        .get("pliCount")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0) as u64;
    (
        format!(
            "Browser video (local hop only):\n{}",
            serde_json::to_string_pretty(&output).unwrap_or_default()
        ),
        pli,
    )
}
