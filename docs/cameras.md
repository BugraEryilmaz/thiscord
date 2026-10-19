# Cameras and the call grid

Join a voice room or accepted private call in the desktop app and press the camera
button in the voice bar. Grant the operating system/WebView camera permission.
The default webcam starts at up to 1280×720, 30 FPS and 1.5 Mbit/s. Microphone mute
and PTT do not stop video. Press the camera button again to stop capture. A missing,
busy or denied device produces an error without ending voice or screen sharing.
Camera choice currently follows the system default; an in-app device selector is
not implemented.

Cameras and screen shares appear together in a grid, including local previews.
Only active media gets a tile; voice-only participants remain in the sidebar.
The grid fills the available call area and adapts its columns to the tile count
and window width. Clicking your connected voice channel opens its room view
without reconnecting. Private calls keep a compact video area above the chat,
leaving message history and the composer visible.
Click a tile for fullscreen; Escape restores the grid. Right-click to hide an
item or change the participant's voice/shared-screen audio levels. Hiding a
remote tile unsubscribes its media; hiding your own tile keeps publishing. Use
Show hidden items to restore it. Clicking a camera/screen participant in a guild
sidebar opens that room's grid. Private-call grids appear in their conversation.

Small overlapping avatars on each video show its current remote viewers. Hover
or keyboard-focus any avatar to see all viewers' names for that stream. These
overlays use no extra layout space and work in fullscreen. Self-preview does not
count as a viewer. The backend
derives camera and screen audiences separately from authorized, live subscriptions;
hiding, deafening, disconnecting, expiry or permission loss removes the viewer.
Presence updates arrive with the call roster, normally within one to two seconds;
abandoned subscriptions expire after three seconds. Viewer lists are only sent
inside the connected call, not in the outside-call sidebar roster.

## Transport and lifecycle

Rust/WASM calls getUserMedia with audio disabled and bounded video constraints.
The WebView encodes H.264 and sends it through a loopback-only encrypted WebRTC
connection to native Rust. No pixels or audio cross Tauri IPC. Native Rust sends
compressed video over the authenticated call's existing encrypted WebRTC session;
the SFU forwards without decoding or transcoding. The server can access media
because these hops terminate encryption; this is not end-to-end encryption.
No new application server, JavaScript pipeline or camera library is used.

Camera tracks have separate publisher/relay SSRCs, publication epochs, viewer
leases, recovery feedback and bounded SFU egress queues. Speak permission grants
camera publication. Both ingress and egress check current access, membership,
deafen state and publication state. Screen audio only follows screen subscriptions.
The wire adds camera_active/camera_epoch and camera_video capability metadata;
old rosters/offers default these to off. Video subscription/feedback kinds default
to screen_video, omitted when serializing screen messages for compatibility.
Deploy the updated backend before enabling cameras in clients.

Camera receiver budgets start at 1.5 Mbit/s and decrease with loss or decoder
pressure. The lowest active viewer budget caps the WebView sender; no viewers
pauses network encoding while local preview continues. This is one common encode,
not simulcast or an aggregate link allocator. Native camera ingress and writes
are bounded. Remote viewers reuse the screen reorder/frame-age limits and native
WebRTC-to-WebView playback bridge. Screens retain their existing GPU encode path.
Local screen preview keeps capture alive but does not upload video without remote
viewers. Hiding it also stops local frame assembly and keyframe requests.

Capture stops on deafen, leaving, switching calls, disconnect/replacement, logout,
window destruction, lost device or expired UI lease. Cancellation stops acquired
tracks even during negotiation; permission requests that complete after cancellation
immediately release their tracks. Initial connection has a ten-second deadline and
prolonged disconnection a ten-second grace period. Camera state is not persisted
or automatically restarted on rejoin. Camera and screen recovery requests use
independent rate limits, with signaling capacity for a full eight-person grid.

## Platform requirements and checks

Windows requires WebView2 camera permission and H.264 WebRTC encoding/decoding.
macOS requires WKWebView media support; Info.plist declares camera usage and the
bundle entitlement permits camera access. Linux depends on the installed WebKitGTK,
GStreamer camera source and H.264/WebRTC codecs. Unsupported environments fail with
an error; there is no silent microphone capture or software application-server
fallback. Existing STUN/TURN configuration applies to the native remote media hop;
the local camera bridge uses neither.

No physical camera is required for the automated tests:

- Shared wire tests cover camera metadata, separate kinds and legacy defaults.
- Backend tests use real encrypted WebRTC peers and synthetic video packets in
  guild and private calls. They cover simultaneous camera/screen/audio delivery,
  hiding screen without losing camera, independent restart epochs and access fences.
- Native desktop tests send synthetic H.264 packets through the actual camera
  loopback bridge and onward to a receiving peer; they exercise cleanup/leases and
  permission rejection. Existing H.264 decode/receive tests cover the common player.
  Receiver regressions cover restoring a hidden stream after a large RTP sequence
  gap and keeping camera congestion feedback within its encoder budget.

Run the backend tests in WSL with TEST_DATABASE_URL for the dedicated test database
and THISCORD_VOICE_BIND=127.0.0.1:0. Do not import production STUN/TURN configuration
into these loopback tests. CI already runs the ignored PostgreSQL voice tests and
the native desktop tests on its Windows/macOS/Linux matrix.

Physical camera permission prompts, unplug/busy-device behavior, visual quality,
fullscreen/context-menu interaction and an eight-person mixed camera/4K-screen load
still require manual acceptance on Windows, macOS and Linux. Automated packet tests
do not establish those runtime or performance results. A virtual camera selected
as the system default can supply moving test video on a machine without a webcam.
