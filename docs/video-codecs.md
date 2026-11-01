# Video codecs, profiles and the client pixel path

Measured 2026-11-01 on the local i7-13700K / RTX 4070 Ti (HDR monitor, 240-nit
SDR white) with synthetic scenes. These are single-machine results, not acceptance
on Intel/AMD adapters, SDR monitors or real networks. Raw probe output is kept in
[benchmarks/screen-codecs-windows-2026-11-01.txt](benchmarks/screen-codecs-windows-2026-11-01.txt).

## One bitstream per layer, whatever the viewers support

The SFU forwards compressed video without transcoding, and every viewer decodes in
its WebView. Supporting several codecs must not turn into one encode per viewer
capability. The rule is therefore:

- A publisher runs **at most the existing two encoders** (primary and low layer;
  see [screen-sharing.md](screen-sharing.md#viewing-demand-and-adaptive-quality)).
- Each live view reports what its local decoder negotiated (`Subscription.h264_high`).
  The SFU plans **one stream-wide `VideoProfile`** that every current, authorized
  viewer of that share decodes (`VideoProfile::plan`) and sends it in `screen_target`.
- One baseline-only viewer selects the compatibility floor for everyone. Leaving,
  hiding, lease expiry or losing access removes that viewer from the plan.
- A profile change rebuilds the encoder exactly like a quality change, so the new
  bitstream starts with SPS/PPS/IDR and viewers switch decoders on a keyframe.
- Capability is descriptive input, never an access grant. The server discards
  `h264_high` from receivers that did not negotiate it, and the existing epoch,
  subscription, access and byte-budget checks are unchanged.

### Wire compatibility

`screen_profiles` is offered by new servers and echoed by new desktop answers (only
alongside `screen_subscriptions`). Only then do views carry `h264_high` and does
`screen_target` carry `profile`. Both fields are omitted at their defaults, because
older peers deny unknown fields; an omitted profile means `h264_baseline`. Old
clients, old servers, bots (OpenH264 decodes baseline only) and cameras (WebView
encoded) keep constrained baseline unchanged.

### Receiver side

The native WebView bridge registers H.264 Constrained High (`640c34`) and High
(`640034`) next to the existing baseline entry. The negotiated answer tells whether
this WebView has a High decoder; only then does the forwarding thread admit High
SPS, which go through the same dimension/reference-picture bounds as baseline plus
8-bit 4:2:0 and scaling-list checks (`shared::h264::parameter_set_profile`). High
frames are written with the WebView's High payload type, baseline frames with the
baseline payload type, so each WebView decoder starts at an IDR. A local preview
whose WebView lacks High keeps its own share at baseline.

## Measurements

`screen_gpu_probe --codecs --adapter=0` (optimized `--profile ci` build) encodes the
same moving FP16 scene at the same CBR target, reads back the encoder input and
decodes with Windows' H.264 decoder MFT (OpenH264 cannot decode High). Luma PSNR:

| Mode | Baseline, driver hint | Baseline, fastest | High, driver hint | High, fastest |
|---|---|---|---|---|
| 1920x1080/60, 10 Mbit/s | 43.65 dB | 39.58 dB | 45.42 dB | 43.89 dB |
| 3840x2160/60, 32 Mbit/s | 41.57 dB | 36.39 dB | 44.07 dB | 42.02 dB |

- **Constrained High vs Baseline:** +1.8 dB (1080p) and +2.5 dB (4K) at the driver
  default speed hint; +4.3/+5.6 dB at the fastest hint. Actual output stayed under
  the target in all arms (9.1–9.9 and 28.1–29.5 Mbit/s).
- **Speed hint:** the driver default (readback 33) is +4.1 dB (1080p) and +5.2 dB
  (4K) over hint 0 on this content. The older 1.6 dB figure in screen-sharing.md
  used a different decoder and frame alignment; both favor the default.
- In the real WGC → encoder → pacer → receive pipeline (`screen_pipeline_probe
  --high`), all 1,786 frames were Constrained High at a sustained 59.35 FPS with no
  drops or loss. The synthetic window then needed noticeably fewer bytes than the
  baseline runs (41 MB vs about 60 MB in 30 s, under the 32 Mbit/s target), but
  content varies between runs; treat that only as an indication.

### Speed hint policy

`SpeedPolicy` keeps the driver default up to the 1080p60 pixel rate and starts at
the fastest hint above it (4K/1440p60), because synchronous 4K60 High at the default
hint measured about 11 ms per frame and the integrated UHD 770 measured little 4K
headroom. If mean MFT service time over 30 frames exceeds 70% of the frame interval,
the session switches to the fastest hint once (live where the driver accepts it,
otherwise one encoder rebuild) and never raises it again, so it cannot oscillate.

## Temporal layers and long-term references

Inventory on this machine (`--codecs`): the NVIDIA and Intel Quick Sync H.264, HEVC
and AV1 encoder MFTs all report `CODECAPI_AVEncVideoTemporalLayerCount`,
`CODECAPI_AVEncVideoLTRBufferControl` and `QualityVsSpeed` support and accept a
1080p60 configuration.

- **Temporal layers (NVIDIA H.264, 2 layers):** accepted (readback 2). 30 of 60 frames
  were non-reference (`nal_ref_idc = 0`) and every frame carried an SVC prefix NAL
  (type 14). The full stream decoded 60/60; with every non-reference frame dropped,
  the remaining 30 base-layer frames decoded with no errors. An SFU could therefore
  halve a weak viewer's frame rate, or lose an upper-layer frame, without an IDR or a
  second encoder. This is not enabled yet: the SFU's layer router, the receive
  queue's loss handling and WebView behavior with prefix NALs need their own change
  and tests.
- **Long-term references (NVIDIA H.264):** `LTRBufferControl` (trust modes 1 and 0,
  one frame) and `MarkLTRFrame` were accepted, but `UseLTRFrame` was rejected
  through `ICodecAPI`. LTR-based loss recovery is therefore not available through
  this path on this driver; IDR recovery remains. Intel/AMD were not tested.

Re-run the probe on Intel/AMD before relying on either feature.

## AV1 as an optional primary layer (scoping)

AV1 is the strongest candidate for better quality per bit, especially for screen
content, and fits the two-layer design without multiplying work:

- **Encoders:** `NVIDIA AV1 Encoder MFT` and `Intel Hardware Accelerated AV1 Encoder
  MFT` are present here and accept a 1080p60 NV12 configuration. Hardware AV1 encode
  generally needs RTX 40, Intel Arc/Meteor Lake or newer, or AMD RDNA3.
- **Transport:** the pinned `rtc-rtp` 0.21 already includes an AV1 payloader and
  depacketizer, so no new dependency is needed for RTP framing.
- **Decoders:** WebView2 (Chromium) negotiates AV1 for WebRTC. WKWebView and
  WebKitGTK support depends on OS/hardware and installed GStreamer elements, so it
  must be detected from the bridge's negotiated answer exactly like High is now.
- **Plan:** extend `VideoProfile` per layer. The primary layer may be AV1 only when
  every viewer routed to it negotiated AV1 *and* the publisher has an AV1 MFT; the
  low layer stays H.264 as the universal fallback. That keeps the ceiling at two
  encoders. A viewer that cannot decode AV1 is routed to the H.264 low layer, or
  the plan falls back to H.264 for both layers when that viewer needs primary quality.

Work required before implementation (multi-week, not started):

1. AV1 equivalents of the H.264-specific code: access-unit assembly, recovery-frame
   (keyframe/sequence header) detection, decoder allocation bounds and the SFU's
   IDR-gated layer switching and recovery-prefix buffering.
2. Per-layer payload types and codec negotiation on the publisher track, the SFU
   relay tracks and the WebView bridge, with legacy defaults preserved.
3. Encoder: AV1 MFT configuration, rate control, keyframe forcing and GOP settings
   (property support differs from H.264), plus the existing probes for AV1.
4. Acceptance on Windows/macOS/Linux viewers and Intel/AMD/NVIDIA publishers.

### Codecs not pursued

- **HEVC:** WebView WebRTC support is inconsistent across the three platforms and
  licensing is complicated; it offers less than AV1 for the same integration cost.
- **VP9:** NVIDIA has no VP9 hardware encoder, and the production path has no CPU
  encoding fallback by design.

## Client pixel path (SDR fast path)

Capture now chooses its surface format per session. SDR displays capture 8-bit
BGRA and a single D3D11 video-processor blit scales and converts to BT.709 NV12. HDR
displays keep the FP16 scRGB tone-map shader. Turning HDR on during an SDR share
restarts capture in FP16 (one IDR); FP16 stays in use if HDR is later turned off, so
a window dragged across mixed displays cannot flap. See
[screen-sharing.md](screen-sharing.md#media-and-dependencies).

`screen_gpu_probe --conversion` times the capture copy plus conversion with D3D11
timestamp queries: 0.182 → 0.051 ms (4K → 4K), 0.140 → 0.046 ms (4K → 1080p) and
0.135 → 0.046 ms (4K → 720p), about 3x less graphics-queue time, and half the
capture texture memory (66.4 → 33.2 MB per 4K texture, up to seven when layered).
Timestamps only cover the graphics queue (a video-processor blit may run on another
engine) and the smooth synthetic scene compresses well, so absolute numbers are
optimistic. The local display is HDR, so live WGC runs here exercised FP16 capture;
SDR capture correctness is covered by the probe's synthetic BGRA levels at three
scales. Live SDR-monitor acceptance remains outstanding.
