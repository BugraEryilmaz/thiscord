# Thiscord roadmap

Foundation and account-management code are implemented; provider/platform acceptance
checks are tracked below. Work roughly in order; the early
media compatibility spike can happen before chat to expose platform constraints.

## 0. Foundation

- [x] Three-package workspace with shared Serde communication contracts.
- [x] Leptos UI and Tauri launcher in one frontend package.
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
- [x] Test invalid credentials, expired/replayed sessions, throttling and OAuth failures.
- [ ] Configure Google web-client credentials and SMTP; complete live provider acceptance.
- [ ] Verify browser handoff and OS-store prompts interactively on Windows/macOS/Linux.
  Windows credential-store round trip and Windows/Linux loopback tests pass locally;
  macOS execution remains a CI/manual check. See docs/accounts.md.
- [ ] Future: confirmed email/username changes and account export.

## 2. Membership and detailed permissions

**Instance** means the hosted deployment; **server/guild** means a community.
A main/instance role administers the deployment. Each guild has independent
roles assigned to its memberships; a user can have different roles in each guild.

- [ ] Instance owner/admin/user permissions and secure first-owner bootstrap.
- [ ] Guild ownership, memberships, role definitions and membership-role assignments.
- [ ] Everyone/default role, multiple-role composition and owner/admin behavior.
- [ ] Role hierarchy, grant/edit restrictions and privilege-escalation prevention.
- [ ] Channel role/member overrides with deterministic allow/deny precedence.
- [ ] Permissions for guild/channel/role management, invites, history, sending,
  editing/deleting messages, moderation and voice join/speak/mute/deafen/move.
- [ ] Central backend permission evaluator; shared permission identifiers/types.
- [ ] HTTP, WebSocket event/subscription and media authorization on every operation.
- [ ] Revoke active socket/voice access immediately on membership/role changes.
- [ ] Role editors, assignment UI and effective-permissions preview.
- [ ] Test cross-guild isolation, overrides, hierarchy, ownership transfer,
  last-owner protection, stale access and concurrent updates.

## 3. Server and channel management

- [ ] Create/edit/delete guilds, list memberships, join and leave.
- [ ] Invite links with expiry/use limits, revocation and join validation.
- [ ] Ownership transfer, kick/ban/unban and administrative audit events.
- [ ] Create/edit/delete/reorder text and voice channels and categories.
- [ ] Guild navigation, channel/member lists and settings views.
- [ ] Cascade/archive behavior and confirmation UX for destructive operations.
- [ ] Transaction and authorization tests for management operations.

## 4. Text chat and real-time transport

- [ ] Shared versioned client/server socket event envelopes and errors.
- [ ] Socket authentication, authorized subscriptions and event rate limits.
- [ ] Heartbeats, disconnect cleanup, reconnect/backoff and resynchronization.
- [ ] Persist/send messages, server IDs/ordering and client deduplication.
- [ ] Cursor-paginated history, permission-checked edit/delete and live updates.
- [ ] History/composer UI with pending/failed/retry states and scroll behavior.
- [ ] Unread markers, mentions, typing indicators and online presence.
- [ ] Safe message rendering, bounded payloads and spam controls.
- [ ] Reconnect, duplicate delivery, channel isolation and channel deletion tests.
- [ ] Later: access-controlled attachments/quotas, search and desktop notifications.

## 5. Frontend audio and media feasibility

- [ ] Spike capture/playback/WebRTC in Tauri on Windows, macOS and Linux; choose
  WebView APIs through Rust bindings or a native Rust audio path based on results.
- [ ] Select compatible maintained Rust media libraries.
- [ ] Enumerate/select input/output devices, persist settings and handle hot-plug.
- [ ] Microphone permission UX, device failures, input levels and test playback.
- [ ] Mute/deafen, per-user volume, voice activation and push-to-talk.
- [ ] Echo cancellation, noise suppression and automatic gain control evaluation.
- [ ] Global push-to-talk permissions and native platform integration.
- [ ] Release devices on leave/logout/suspend/shutdown; test headset changes.

## 6. WebRTC transport and signaling

- [ ] Shared join/leave/offer/answer/ICE contracts over authenticated signaling.
- [ ] Peer lifecycle, SDP negotiation, ICE candidates/restarts and reconnects.
- [ ] Opus negotiation, DTLS-SRTP and connection-quality metrics.
- [ ] STUN/TURN deployment, short-lived relay credentials and NAT traversal tests.
- [ ] WSL/Windows/router public/private addressing and UDP port mapping.
- [ ] Validate voice permissions and reject stale/replayed signaling sessions.
- [ ] Restrictive NAT, relay fallback, packet loss/jitter and device-change tests.

## 7. Backend Rust SFU

- [ ] Evaluate Rust SFU libraries/implementation and record the architecture choice.
- [ ] Rooms, publishers/subscribers and participant lifecycle in backend modules.
- [ ] Terminate WebRTC transports and forward authorized RTP/RTCP audio streams.
- [ ] RTCP feedback, congestion, slow receivers and per-room resource limits.
- [ ] Permission-controlled admission/eviction tied to guild roles, channels and bans.
- [ ] Cleanup abandoned sessions, metrics and graceful room draining on shutdown.
- [ ] Multi-party load tests and capacity/bandwidth budgets.
- [ ] Document encryption boundaries: SFU transport encryption is not automatic E2EE.

## 8. Voice channels end to end

- [ ] Join/leave/move voice channels, participants UI and synchronized presence.
- [ ] Wire frontend audio to SFU streams; speaking indicators and quality UI.
- [ ] User mute/deafen and moderator mute/deafen/move/disconnect controls.
- [ ] Capacity, join/speak permissions and live permission-change enforcement.
- [ ] Recovery after network changes, suspend/resume, restart and deleted channels.
- [ ] Multi-user acceptance tests on Windows/macOS/Linux with real microphones.

## 9. Hosting and releases

- [ ] DNS for `thiscord.com.tr`/`api.thiscord.com.tr`, HTTPS and WSS.
- [ ] Reverse proxy, WSL service startup/supervision, Windows networking/firewall,
  SFU/TURN UDP exposure; keep PostgreSQL private.
- [ ] OAuth redirect registration and per-environment CORS/CSP configuration.
- [ ] Database backups/restore tests and migration/rollback deployment procedures.
- [ ] Logs/metrics, resource/disk limits and operations runbook.
- [ ] App icons/installer metadata and Windows/macOS/Linux installers.
- [ ] Signing/notarization and authenticated update distribution.
- [ ] Accessibility, keyboard navigation, failure recovery and platform smoke tests.
- [ ] Review session security, authorization, file access and media permissions.
