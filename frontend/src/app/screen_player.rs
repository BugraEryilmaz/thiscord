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

pub async fn run(video: HtmlVideoElement, watch: Watch, message: RwSignal<String>) {
    loop {
        while !visible() {
            gloo_timers::future::TimeoutFuture::new(250).await;
        }
        match connect(video.clone(), watch).await {
            Ok(player) => {
                let mut pending = 0;
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
                        json!({"watch":watch,"visible":true}),
                    )
                    .await
                    .unwrap_or(false)
                    {
                        break;
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
