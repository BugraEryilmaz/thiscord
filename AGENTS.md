# Project conventions

## Scope and stack

- Build a personal Discord-style app associated with `thiscord.com.tr`. Ownership
  of the domain does not mean DNS or deployment is configured in this repository.
- Use Rust application logic, Tauri 2, Leptos CSR/WASM, Trunk, Axum/Tokio and Diesel
  with locally hosted PostgreSQL. No Node.js pipeline or handwritten JS/TS logic.
  HTML/CSS, TOML/JSON, SQL migrations and generated binding glue are expected.
- Style the frontend with Tailwind CSS 4 through Trunk's standalone CLI integration.
  Pin its version in `frontend/Trunk.toml`; keep CSS imports, source scanning and
  theme tokens in `frontend/style.css`. Use complete utility class strings in Rust
  components so the scanner can detect them. Do not introduce npm or a CDN runtime.
- Target native Windows, macOS and Linux clients. Run backend/PostgreSQL under WSL.
- Keep scaffolding minimal. Unchecked items in TODO.md are not implemented features.

## Package boundaries

- Preserve exactly three packages: frontend, backend, shared. Frontend contains
  the WASM UI and native Tauri binaries, separated by targets/features. Do not add
  a fourth Tauri package merely to follow a generator's default layout.
- Dependency direction is `frontend -> shared <- backend`; neither side imports
  the other. Shared owns communication DTOs, IDs, wire errors, permission identifiers,
  WebSocket and signaling events as they are introduced. Use stable Serde formats.
- Keep shared native/WASM portable, without Axum, Diesel, Leptos, Tauri, OS APIs
  or database models. Do not duplicate transport contracts on either side.
- Backend owns database models/schema, migrations, authentication, authorization
  and secrets. Map database entities to DTOs; never serialize password hashes,
  session secrets or internal database errors to clients.
- Frontend owns views, UI state, typed API access, native integrations and audio.
  UI permission checks are for usability; backend enforcement is authoritative.
- Add feature modules when needed. Keep SFU/WebRTC in backend modules initially
  and avoid speculative dependencies or empty service layers.

## API, permissions and media

- Version JSON HTTP routes under `/api/v1`. Define shared contracts before their
  consumers. Use authenticated WebSockets for events/signaling and WebRTC for media.
- Follow `docs/api.md` for UUID newtypes, UTC timestamps, bounded keyset pagination,
  validation and the error envelope. Keep request IDs in errors and logs, and keep
  liveness independent of database readiness. Map future input-extractor failures
  into the shared wire-error format instead of returning framework-specific text.
- Distinguish deployment-wide instance roles from community server/guild roles.
  Guild roles apply only to membership in that guild. Specify ownership, hierarchy,
  multiple-role composition and channel override precedence before role editors.
- Check permissions for HTTP actions, socket subscriptions/events and SFU joins.
  Membership/role changes must also revoke ongoing subscriptions and voice access.
- Conventional login and Google OIDC attach to a single account model. Keep provider
  secrets on backend; use system-browser login with PKCE/state/nonce and a reviewed
  desktop callback. Never collect Google passwords in the app.
- Select Rust WebRTC/SFU libraries after a three-platform compatibility spike.
  Do not silently substitute a non-Rust application server. Document native codec,
  WebView and STUN/TURN infrastructure dependencies and encryption boundaries.

## Persistence and configuration

- Store reviewed migrations under `backend/migrations`, including rollback behavior.
  Generate `backend/src/schema.rs` using Diesel after schema changes.
- Startup applies embedded migrations; `--migrate-only` is available for setup.
  Local PostgreSQL lives in the `kali-linux` WSL distribution; connection secrets
  are in ignored `backend/.env`. See `docs/database.md` before database operations.
  Integration tests use `TEST_DATABASE_URL` and their own temporary schema.
- Use a bounded pool. Run synchronous Diesel queries and pool checkout through
  `spawn_blocking`, off Tokio request workers. Use transactions for related writes;
  do not hold connections across unrelated async waits.
- Environment variables configure backend; .env is local-only. Commit examples,
  never secrets. PostgreSQL is private and never accessed directly by clients.
- Keep Tauri capabilities minimal, CSP scoped and CORS origins explicit. Do not
  log credentials, tokens or private messages.
- Frontend URL is currently compile-time `THISCORD_API_URL`. Any later runtime
  configuration must preserve URL validation and CSP compatibility.

## Development

- Use pinned Rust and commit the root Cargo.lock. Separate Windows/WSL target dirs.
- Keep the Leptos and native VS Code workspaces separate: WASM analysis/checks
  must select only `thiscord-ui`; native analysis enables the Tauri `desktop`
  feature. Match build-script and on-save check commands when changing either.
- Run formatting, targeted compilation/Clippy and relevant tests. Check UI for
  WASM, backend in WSL with libpq, and launcher on native desktop hosts. Do not
  build every workspace feature for WASM.
- Add meaningful tests for auth, permissions/isolation, persistence, wire
  compatibility and media lifecycle when implementing those features.
- Report unavailable checks; a Windows build does not establish Linux/macOS
  runtime compatibility. Keep README and TODO accurate; stubs are not completed features.
- Keep the CI workflow's shared/backend tests (including ignored PostgreSQL tests),
  WASM build and native desktop matrix current when changing build requirements.
