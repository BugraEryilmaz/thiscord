# Thiscord roadmap

Foundation and account-management code are implemented; provider/platform acceptance
checks are tracked below. Work roughly in order; the early
media compatibility spike can happen before chat to expose platform constraints.

## 0. Foundation

- [x] Browser `/admin` dashboard with current instance-owner/admin authorization, host metrics and live voice diagnostics.
- [ ] Deploy the admin bundle to the VPS and verify production password/Google login and real-client telemetry.

- [x] Three-package workspace with shared Serde communication contracts.
- [x] Leptos UI and Tauri launcher in one frontend package.
- [x] Persistent opt-in close-to-tray setting with Open/Quit tray actions and macOS Dock restore.
- [ ] Verify close-to-tray, active calls/screen sharing, restore, restart persistence and explicit quit interactively on Windows/macOS/Linux (including Linux without a tray host).
- [x] Tailwind CSS styling through Trunk, with Rust source scanning and shared theme tokens.
- [x] Axum health endpoint consumed by the UI, optional Diesel pool and migration setup.
- [x] Setup documentation and project rules in AGENTS.md.
- [x] Set up local PostgreSQL in WSL and verify connectivity/first migration.
- [x] Add typed IDs, wire errors, validation, pagination and timestamps as needed.
- [x] CI: format/Clippy, backend/shared tests, WASM build and native desktop matrix.
- [x] Database readiness endpoint and request IDs, separate from process liveness.

## 1. Accounts and login: backend and frontend

- [x] Migrations for accounts, unique usernames/emails, credential identities and sessions.
- [x] Registration, standard password login, logout and current-user API/UI.
- [x] Argon2id password hashing, input validation and registration/login rate limits.
- [x] Email verification, password change, forgotten-password and reset flows.
- [x] Session expiry, rotation, revocation, multi-device management and logout-all.
- [x] Desktop session storage using OS credential storage through Rust integration.
- [x] Google OIDC authorization-code login with PKCE, state and nonce validation.
- [x] System-browser login and secure desktop loopback implementation targeting
  on all three OSes, including cancellation, failures and replay rejection.
- [x] Validate provider issuer/audience/signature; keep Google secrets on backend.
- [x] Link/unlink login identities after reauthentication; prevent takeover through
  unverified email matching and removal of the last usable login method.
- [x] Profile/settings and account deletion behavior (display name/bio; identifiers immutable).
- [x] Upload, replace and remove profile pictures in Settings; persist normalized images and display them in live voice rosters and the overlay.
- [x] Test invalid credentials, expired/replayed sessions, throttling and OAuth failures.
- [ ] Configure Google web-client credentials and Resend; complete live provider acceptance.
- [ ] Verify browser handoff and OS-store prompts interactively on Windows/macOS/Linux.
  Windows credential-store round trip and Windows/Linux loopback tests pass locally;
  macOS execution remains a CI/manual check. See docs/accounts.md.
- [ ] Future: confirmed email/username changes and account export.

## 2. Membership and detailed permissions

**Instance** means the hosted deployment; **server/guild** means a community.
A main/instance role administers the deployment. Each guild has independent
roles assigned to its memberships; a user can have different roles in each guild.

- [x] Instance owner/admin/user permissions and secure first-owner bootstrap.
- [x] Guild ownership, memberships, role definitions and membership-role assignments.
- [x] Everyone/default role, multiple-role composition and owner/admin behavior.
- [x] Role hierarchy, grant/edit restrictions and privilege-escalation prevention.
- [x] Channel role/member overrides with deterministic allow/deny precedence.
- [x] Shared permission identifiers for guild/channel/role management, invites, history, sending,
  editing/deleting messages, moderation and voice join/speak/mute/deafen/move.
- [x] Central backend permission evaluator; shared permission identifiers/types.
- [x] Authorize every implemented membership/role/channel-management HTTP operation.
- [x] Authorize chat HTTP operations and WebSocket subscriptions/deliveries.
- [x] Authorize signaling admission and every SFU publisher/receiver route.
- [x] Invalidate active sockets on membership/role/session changes (single backend process).
- [x] Revoke active voice access after membership/role/session changes.
- [x] Role editors, assignment UI and effective-permissions preview.
- [x] Test cross-guild isolation, overrides, hierarchy, ownership transfer,
  last-owner protection, stale access and concurrent updates.

See docs/permissions.md for local owner bootstrap, precedence, bounded guild sizes
and single-process chat/media enforcement.

## 3. Server and channel management

- [x] Create/edit/delete guild HTTP commands, list memberships, join by ID/password and leave.
- [x] Joined-server rail with circular initials avatars, names on hover, create/join dialogs.
- [ ] Custom server images and password-change settings UI.
- [ ] Invite links with expiry/use limits, revocation and join validation.
- [x] Ownership transfer, kick/ban/unban and per-server moderation controls.
- [x] Timeouts with expiry/removal, chat/voice enforcement and join-bypass protection.
- [ ] Administrative audit events and moderation reasons.
- [ ] Create/edit/delete/reorder text and voice channels and categories.
- [ ] Guild navigation, channel/member lists and settings views.
- [ ] Cascade/archive behavior and confirmation UX for destructive operations.
- [ ] Transaction and authorization tests for management operations.

## 4. Text chat and real-time transport

- [x] Shared versioned client/server socket event envelopes and errors.
- [x] Socket authentication, authorized subscriptions and event rate limits.
- [x] Heartbeats, disconnect cleanup, reconnect/backoff and resynchronization.
- [x] Persist/send messages, server IDs/ordering and client deduplication.
- [x] Cursor-paginated history, permission-checked edit/delete and live updates.
- [x] History/composer UI with pending/failed/retry states and scroll behavior.
- [x] Unread markers, mentions, typing indicators and online presence.
- [x] Safe message rendering, bounded payloads and spam controls.
- [x] Reconnect, duplicate delivery, channel isolation and channel deletion tests.
- [ ] Later: access-controlled attachments/quotas, search and desktop notifications.

## 5. Frontend audio and media feasibility

- [x] Native CPAL capture/playback, per-stream SPSC ring buffers and Opus codecs.
- [x] Windows/Linux encrypted WebRTC software probes; three-OS CI build/test matrix.
- [x] Select Rust media libraries and document native dependencies in docs/audio.md.
- [x] Enumerate/select devices, persist settings and refresh device lists.
- [x] Bound device recovery to three selected-device and three default-device attempts;
  retain voice during retries and show best-effort Windows audio-session processes.
- [x] Resolve missing saved devices to the system default independently on join/recovery;
  preserve preferences and announce first-open fallback, with deterministic regression tests.
- [x] Quarantine potentially blocked stream cleanup, separate control/media queues,
  use native voice heartbeats and bound worker panic restarts.
- [x] Persist rotating audio diagnostics, callback/watchdog/cleanup state and safe
  panic metadata; expose a logs-folder button independent of the audio worker.
- [x] Make local diagnostic logging opt-in through a persistent Audio & voice toggle.
- [ ] Validate recovery/default microphone fallback and busy-device diagnostics on
  real Windows, macOS and Linux hardware, including exclusive-mode conflicts.
- [x] Microphone permission/error UX, input levels and bounded playback/loopback tests.
- [x] Mute/deafen, independent speaker volume, voice activation and in-app push-to-talk.
- [x] Optional Rust echo cancellation, noise suppression and automatic gain processing.
- [x] Optional global Ctrl+Shift+Space integration with conflict/error reporting.
- [x] Retire devices on leave/logout/window destruction and lease expiry; stop retired
  callbacks while potentially blocked driver cleanup runs separately.
- [ ] Physical microphone/headset, suspend and acoustic acceptance on all three OSes.
- [ ] macOS runtime/permission spike; Wayland global shortcut portal integration.
- [ ] Arbitrary sample-rate resampling and Bluetooth hands-free format support.

## 5a. Voice isolation and speech quality

Research, model costs and proposed acceptance budgets: [voice-isolation.md](docs/voice-isolation.md).
DeepFilterNet3 denoising is available as an experimental option. Personalized
speaker isolation and physical acoustic acceptance remain future work.

- [ ] Establish reproducible acoustic/listening baselines against current Sonora
  processing and Discord, with matched devices, levels and processing settings.
- [ ] Build an opt-in fixture/benchmark harness for fan/keyboard/music noise,
  nearby/TV speech, overlapping speakers, target absence and acoustic double-talk;
  include Turkish/English, quiet speech and different microphones/rooms.
- [x] Add optional DeepFilterNet3 at 48 kHz behind replaceable ordered capture
  stages, with Sonora fallback selection, live bypass and persisted model choice.
- [x] Add deterministic noise/speech, reset, bypass, composition and queue-gap
  regressions; offline CPU/latency/quality benchmarks and three-OS CI artifacts.
- [ ] Compare nnnoiseless and GTCRN against the measured DeepFilterNet3 baseline,
  including resampling delay and lost high frequencies.
- [x] Audit public SpeakerBeam-SS sources/checkpoints for live integration; record
  the streaming and model-availability blockers in [voice-isolation.md](docs/voice-isolation.md#speakerbeam-ss-feasibility-review-2026-09-30).
- [ ] Obtain a compatible streaming extractor/enrollment checkpoint and reference
  with redistribution terms; the reviewed public reimplementation uses global
  time normalization and lacks a stateful inference path. No live integration yet.
- [ ] Integrate an optional personalized `CaptureStage` with local enrollment,
  bounded resampling/state and independently selectable DeepFilterNet3 composition.
- [ ] Verify reference parity, prefix causality, chunk-size invariance, target
  absence/overlap, resets/bypass and urgent controls; benchmark enrollment and
  continuous extraction separately on all three desktop OSes.
- [ ] Evaluate reference-aware neural echo suppression/AEC against AEC3 for
  double-talk and nonlinear speaker echo; keep external background speech tests separate.
- [x] Apply device/model changes during voice without reconnecting; retain transport,
  roster and volumes, with bounded background preparation and failure/cancellation tests.
- [ ] Validate live device/model switching and audible adaptation on Windows, macOS
  and Linux hardware, including exclusive drivers and unplugging during a change.
- [x] Add opt-in bounded echo debug recording of playback, raw microphone and
  gated encoder input, timing/settings metadata and offline Rust DSP replay.
- [ ] Validate diagnostic capture/replay with consented real echo samples on
  Windows, macOS and Linux; compare warmup, double-talk and local speech retention.
- [x] Extend Sonora with a mono residual-estimator injection point and add optional
  Rust REE v2 inference, local model selection, reset/bypass and failure handling.
- [x] Validate recurrent inference against LiteRT; add neural echo regressions and
  conventional/neural AEC + DeepFilterNet3 comparison benchmarks.
- [ ] Verify neural residual model provenance and distribution terms for the
  owner-requested bundled model; hash-pinning does not establish a model license.
- [ ] Compare neural/conventional residual estimates for double-talk, nonlinear
  echo, path changes and resets; benchmark full pipeline cost on three platforms.
- [ ] Record pinned model/runtime versions, weight hashes, code/weight licenses,
  training-data terms, maintenance status and Windows/macOS/Linux compatibility.
- [ ] Compare release-build CPU/RTF, p50/p95/p99 processing times, algorithmic delay,
  peak memory, startup, underruns and battery/thermal behavior on all three OSes.
- [x] Integrate DeepFilterNet3 on the bounded native capture worker; preserve
  AEC reference timing, per-stream rings, urgent mute/PTT/leave and SFU forwarding.
- [ ] Add optional local voice-profile enrollment, quality checks, re-enrollment
  and deletion; protect embeddings and keep samples out of logs, IPC and backend.
- [ ] Add explicit noise-suppression versus personal-isolation modes, strength
  controls and bounded listening tests; handle missing profiles, model failures,
  CPU overload and target absence without silently transmitting unintended speech.
- [ ] Test state resets, silence, clipping, queue gaps, device changes, suspend,
  simultaneous speakers and eight-participant calls under CPU contention.
- [ ] Set release quality/performance gates from measurements; run blind A/B
  acceptance and an opt-in rollout before changing defaults or claiming improvement.
- [ ] Later: evaluate quantization/distillation and microphone-array beamforming
  only where measured costs or compatible hardware justify the extra complexity.

## 6. WebRTC transport and signaling

- [x] Shared versioned authenticated join/leave/offer/answer/state/error contracts.
- [x] Bounded peer negotiation, gathered SDP ICE candidates and disconnect cleanup.
- [x] Opus negotiation and per-hop DTLS-SRTP; encrypted two-hop probe.
- [x] Configurable STUN/TURN and backend-generated short-lived TURN credentials.
- [x] Validate current sessions/voice permissions; reject duplicate negotiation.
- [x] Document WSL/Windows UDP addressing and firewall requirements.
- [x] Automatic full peer reconnection with fresh offers and relay credentials.
- [ ] In-place ICE restart and proactive long-call relay credential refresh.
- [x] Docker coturn deployment, shared-secret configuration and local relay tests.
- [ ] Restrictive NAT, forced WebRTC relay fallback and off-site public UDP acceptance.
- [ ] Packet-loss/jitter network trials, connection quality metrics and adaptive bitrate.

## 7. Backend Rust SFU

- [x] Native Rust WebRTC SFU architecture recorded in docs/audio.md.
- [x] In-memory rooms, publisher/subscriber slots and participant lifecycle.
- [x] Forward encoded audio without transcoding; per-hop RTCP interceptors.
- [x] Bounded ingress/egress, slow-receiver deadlines and eight-person room limits.
- [x] Permission-controlled admission and eviction tied to channel/session changes.
- [x] Abandoned connection cleanup and conservative access-change invalidation.
- [x] Document bandwidth estimate and transport encryption versus E2EE.
- [ ] Sustained multi-party load tests and measured capacity/congestion budgets.
- [x] Opt-in Rust voice load generator and disposable benchmark server; per-stream
  delivery/latency reports and loopback smoke checks (docs/voice-load.md).
- [ ] Production metrics and coordinated graceful room draining on shutdown.

## 8. Voice channels end to end

- [x] Join/leave and move yourself between voice channels; live participant list.
- [x] Connect native capture/playback to SFU streams with per-speaker volume sliders.
- [x] User mute/deafen enforced locally and by the server; persistent voice controls.
- [x] Capacity, join/speak permissions and live permission-change enforcement.
- [x] Real-peer forwarding, channel isolation, self-mute/deafen, revocation,
  Speak denial, publisher slot reuse, deleted-channel and cleanup integration tests.
- [x] Device failures use bounded local recovery; access failures stop audio;
  transient network/ICE/signaling failures
  reconnect with bounded backoff, fresh authorization and cancellable media leases.
- [x] Moderator voice disconnect with hierarchy enforcement and targeted transport revocation.
- [ ] Moderator mute/deafen/move-to-channel controls with hierarchy enforcement.
- [x] Desktop voice overlay with participant roster, speaking activity and focus visibility.
- [ ] In-app remote speaking indicators and connection-quality UI.
- [ ] Overlay runtime acceptance in games, mixed-DPI monitors, macOS Spaces and Linux compositors.
- [x] Automatic transport reconnection after network failure or server restart.
- [ ] Validate real network switching/server outages across native hosts; support
  explicit local device suspend/resume recovery without unexpected capture.
- [ ] Multi-user acceptance on Windows/macOS/Linux with real microphones.

## 8a. Screen sharing (Windows first)

- [x] Native Windows screen/window capture, explicit source picker and persistent stop control.
- [x] Selectable 720p/1080p/1440p/4K and 15/30/60 fps targets, default 1080p/30.
- [x] Windows system-audio loopback excluding Thiscord, separate from microphone mute/PTT.
- [x] H.264/Opus WebRTC tracks, permission-gated SFU forwarding and WebView video player.
- [x] In-app fullscreen viewing and separate saved screen share volume/mute controls.
- [x] Bounded media queues, loss recovery, stop/reconnect cleanup and wire/codec/SFU regression tests.
- [ ] Two-client Windows capture/audio acceptance, mixed-DPI/protected windows and suspend/network checks.
- [ ] macOS/Linux publishing and OS permission acceptance; portable receiving code is included.
- [ ] Stereo system audio, adaptive bitrate, demand-based subscriptions and measured multi-share capacity.
- [x] Continuous Windows Graphics Capture and WebRTC video-element presentation without JPEGs.
- [x] GPU FP16 capture/copy, scaling, HDR-to-SDR tone mapping, NV12 conversion and
  same-adapter Media Foundation surface encoding; synthetic NVIDIA GPU validation.
- [x] Native/SFU/browser diagnostics, measured FPS and copyable bounded event history.
- [x] Bounded packet reordering, negotiated/authenticated keyframe feedback, byte
  pacing and bounded recovery queues with loss/wrap/backlog/permission tests.
- [x] Correct H.264 HRD buffer units, report driver acceptance and gate raw encoding
  on queued/in-flight bytes; synthetic large-frame backlog regression and NVIDIA probe.
- [ ] Live HDR/SDR capture acceptance across Intel/AMD/NVIDIA and mixed-HDR displays.
- [ ] Measured end-to-end 30/60 fps, high-resolution performance and real WebView
  hardware-decoder verification on Windows/macOS/Linux.

See [screen-sharing.md](docs/screen-sharing.md) for limits and remaining platform checks.

## 9. Hosting and releases


- [x] Direct backend HTTPS/WSS using Let's Encrypt PEM files, TLS tests and SIGHUP reload.
- [ ] Finalize public hostname/DNS, browser hosting and certificate renewal automation.
- [ ] Reverse proxy, WSL service startup/supervision, Windows networking/firewall,
  SFU/TURN UDP exposure; keep PostgreSQL private.
- [ ] OAuth redirect registration and per-environment CORS/CSP configuration.
- [ ] Database backups/restore tests and migration/rollback deployment procedures.
- [ ] Logs/metrics, resource/disk limits and operations runbook.
- [x] Installer metadata/configuration and tagged Windows/Linux/macOS release pipeline.
- [x] Signed updater artifacts, complete release feed and verification before publishing.
- [x] Automatic launch/periodic checks with explicit install/restart and voice protection.
- [ ] Configure GitHub signing secrets and run the first installer release pipeline.
- [ ] Windows Authenticode and Apple Developer ID/notarization configuration.
- [ ] Initial-install and version-to-version update acceptance on all three OSes.
- [ ] Accessibility, keyboard navigation, failure recovery and platform smoke tests.
- [ ] Review session security, authorization, file access and media permissions.
