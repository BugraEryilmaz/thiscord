# Accounts and sessions

Start the backend with `backend/run-wsl.ps1` and the desktop with `cargo tauri dev`
from `frontend/`. Restart an already-running backend after updating the Rust code.
The account migration runs automatically; it preserves the installation record.
The migration rollback permanently removes account data and must not be used to
restart development. Passwords, session tokens and provider secrets never belong
in shared models, logs or checked-in configuration.

## Local walkthrough

1. The login screen contains only sign-in, with **Sign up** and **Forgot password?**
   text buttons opening modal dialogs. Create an account using **Sign up**.
   Usernames are normalized to lowercase ASCII and
   accept 3–32 letters, numbers or underscores. Emails are lowercase ASCII,
   validated and unique. Passwords need 12 characters and may use up to 1024 bytes.
2. Within five seconds the email worker writes a `.txt` message to `backend/.mail/`.
   Open the verification URL in that local file. With Resend or SMTP configured, click the
   link directly in your email. It automatically verifies the address and opens a
   confirmation page; there is no verification-code input. Return to Thiscord to
   refresh your status automatically, or use **Refresh account**. Tokens are never
   printed in the terminal. The directory is ignored by Git. On Linux, newly
   created directories/files use 0700/0600; Windows-mounted files inherit Windows
   ACLs. Set `MAIL_DIRECTORY` to a private Linux directory if needed.
3. **Forgot password** sends a code only for verified accounts, with the same
   response for absent accounts. In that dialog, select **I already have a reset code**
   to finish resetting the password. Verification links and reset codes expire
   after 30 minutes, are single-use and are replaced on resend.
   The worker removes expired mail files and database codes. Production email
   already delivered to an inbox cannot be recalled.
4. **Reauthenticate** before changing/adding a password, linking/unlinking Google,
   or deleting your account. Confirmation lasts five minutes. Setting a password
   also requires a verified email. Changes/reset revoke every existing session.
5. **Devices & identities** lists active sessions and lets you revoke a device
   or sign out everywhere. Removing an identity revokes all sessions and cannot
   remove the last login method. A Google-only account can add a password after
   Google reauthentication, or through verified-email recovery.

Profiles support display name, bio and a profile picture. Open **Settings > Profile**
to choose a PNG, JPEG or WebP up to 2 MiB and 4096 × 4096 pixels, or remove the current
picture. Choosing a file saves immediately. The backend applies image orientation,
center-crops/resizes to 256 × 256 and re-encodes a static PNG without the original
metadata. Active voice participants and the desktop overlay update within the next
roster refresh (normally two seconds), without leaving the call. Initials remain
visible when no picture is set or loading fails.

`set_avatar` on `POST /api/v1/account` takes `image_base64` (standard padded base64,
or `null` to remove). Every signed-in account may change only its own picture;
neither a verified email nor a guild/instance role is required. The existing account
lock, session validation, rate limits and four bounded blocking workers apply.
Uploads are limited to 2 MiB decoded, with decoder dimensions and memory bounded.
Account request bodies allow up to 2,797,228 bytes for encoded images and JSON.
`avatar_id` is optional in account, voice participant and overlay metadata, so older
responses still decode. Only the ID travels in roster/IPC updates, not image bytes.

`GET /api/v1/avatars/{avatar_id}` serves presentation media without a bearer token
so the overlay does not need session access. IDs are random UUIDs, exposed with the
profile/authorized voice roster; there is no image listing or account-ID lookup.
Anyone given the image URL can view it. Every replacement gets a new ID, and the
old URL returns 404. Images use `image/png`, `nosniff`, no referrer in clients, and
a private five-minute browser cache. Removal/account deletion deletes the stored
image; previously downloaded/cached copies cannot be recalled. The migration stores
one bounded image per account in PostgreSQL; rollback drops only these pictures.

Username and email are immutable
in this version; changing verified identifiers needs a separate confirmation flow.
Permanent deletion requires recent reauthentication and typing the username. It
deletes the profile, identities, sessions, codes, pending OAuth attempts and queued
email through foreign-key cascades. Database backups and already delivered email
have their own retention policy. Instance owners must transfer ownership, and guild
owners must transfer or delete their guilds first. Other memberships and role
assignments cascade on account deletion. See [permissions.md](permissions.md).
Authored chat content is retained with an anonymized Deleted account author label;
guild/channel deletion removes that channel's chat history. See [chat.md](chat.md).

## Email delivery with Resend

Set these values in ignored `backend/.env`, then restart the backend:

```dotenv
MAIL_MODE=resend
RESEND_API_KEY=re_xxxxxxxxx
RESEND_FROM=onboarding@resend.dev
```

Replace `re_xxxxxxxxx` with your real Resend API key locally. The backend uses the
official `resend-rs` SDK over HTTPS; the key never goes to the frontend. Verification
and password-reset messages are sent to the account's email address, with both HTML
and plain text. No provider response or message content is logged. Missing or
placeholder keys disable sending until configured and the backend is restarted.

The testing sender `onboarding@resend.dev` can only send to the email associated
with your Resend account. To send to other users, verify your domain in Resend and
set `RESEND_FROM="Thiscord <noreply@thiscord.com.tr>"` (or a sender on the domain you
verified). See [Resend's testing-domain restrictions](https://resend.com/docs/knowledge-base/403-error-resend-dev-domain).
Domain ownership alone does not configure the required DNS records.

The worker retries queued mail every five seconds until its 30-minute expiry.
Each outbox message uses a stable Resend idempotency key, including after a restart.
Messages are removed from the queue only after a successful send. API calls have a
15-second timeout and run without holding a database connection. Run one mail
worker per deployment until database claims/multi-worker delivery are added.
Use `MAIL_MODE=file` for offline development or `MAIL_MODE=smtp` for the SMTP option.

Set `PUBLIC_BACKEND_URL` to the origin users can reach, such as
`https://api.thiscord.com.tr`; the local default is `http://localhost:3000`.
Verification emails link to `/api/v1/account/verify-email?token=...` on that origin.
The backend never constructs links from untrusted request Host headers. HTTP is
allowed only for localhost development. Exclude query strings on this route from
reverse-proxy logs. Verification responses use no-store and no-referrer headers;
HEAD requests do not consume the link. Resend verification from the profile page
to replace any older code-only email.

### Optional SMTP delivery

Set `MAIL_MODE=smtp`, `SMTP_HOST`, `SMTP_USERNAME`, `SMTP_PASSWORD` and `SMTP_FROM`
in ignored `backend/.env`, then restart the backend. SMTP uses TLS with certificate
validation on port 465. The worker retries a durable database outbox every five
seconds and drops messages once their 30-minute codes expire. Delivery is at least
once: a crash after send and before acknowledgement can duplicate an email. Run
one mail worker per deployment until database claims/multi-worker delivery are added.
File mode is deliberately a local development default, not an email provider.

## Google sign-in setup

Create a Google OAuth **Web application** client. Keep `GOOGLE_CLIENT_ID` and
`GOOGLE_CLIENT_SECRET` on the backend. Register the exact `GOOGLE_REDIRECT_URL`:

- Local: `http://localhost:3000/api/v1/account/google/callback`.
- Production: `https://thiscord.com.tr/api/v1/account/google/callback`.

In Google Cloud, open **Google Auth Platform > Clients** (or **APIs & Services >
Credentials**), select the Web application client whose ID matches the backend's
`GOOGLE_CLIENT_ID`, and add that URL under **Authorized redirect URIs**. The
consent screen's **Authorized domains** entry `thiscord.com.tr` does not register
a redirect URI. Authorized JavaScript origins are also a separate setting.

For `redirect_uri_mismatch`, compare the `redirect_uri` in Google's error details
with the selected client's registered URI and the running backend's
`GOOGLE_REDIRECT_URL`. Scheme, hostname, path, case and trailing slash must match
exactly. Production uses HTTPS, no `api.` subdomain and no trailing slash. Save
the console changes and start a fresh sign-in attempt. Restart the backend if
you change its environment; a console-only change does not require a client
release. Do not share the full authorization URL (it includes state and PKCE
parameters) or client secrets while troubleshooting. See
[Google's redirect URI requirements](https://developers.google.com/identity/protocols/oauth2/web-server#httprest).

Use a Web application OAuth client even for the installed desktop application:
Google returns to the backend, not directly to the desktop's ephemeral loopback
listener. Do not register that changing local port as Google's redirect URI.

Configure the consent screen/test users as required by your Google project.
Missing configuration produces a safe unavailable response; there is no fake
Google login. This implementation uses the [`openidconnect` crate](https://docs.rs/openidconnect/4.0.1/openidconnect/)
and [Google's OIDC flow](https://developers.google.com/identity/openid-connect/openid-connect).

The desktop binds an ephemeral `127.0.0.1` port with a random path before opening
the system browser. Google redirects to the backend's registered callback, which
verifies the authorization code and redirects a completion notification to that
loopback path. No credentials travel to the local listener. The desktop redeems
an independent random ticket through the account API; that ticket never enters
the browser URL. Browser preview displays a link and polls the same ticket without
a loopback redirect. Cancellation and five-minute expiry discard the attempt;
late/replayed provider callbacks cannot exchange it again. Cancelling after a
completed identity operation does not undo the operation.

The backend uses PKCE S256, random state and nonce, Google's HTTPS discovery/JWKS,
and validates signature, issuer, audience, expiry, nonce, verified email and
`at_hash` when present. Reauthentication additionally requires recent `auth_time`.
Provider access/refresh tokens are not persisted. Only the Google subject identifies
a linked account: an email collision fails, even if Google reports it verified.
Sign into the existing account, reauthenticate, then explicitly link Google.

## Session model and desktop storage

- Random 256-bit bearer tokens; only SHA-256 token hashes persist in PostgreSQL.
- Absolute expiry: 30 days. Idle expiry: seven days. Rotation does not extend either
  deadline; it happens on desktop restore and is also an explicit API operation.
- A consumed token cannot be reused; replay revokes that device's session, including
  its replacement token. Rotation is serialized. A lost rotation response requires
  signing in again. Do not reuse one device token across independent app instances.
- Up to 20 active devices. New logins evict the oldest when that limit is reached.
- Windows Credential Manager, macOS Keychain and Linux Secret Service store desktop
  tokens using Rust [`keyring`](https://docs.rs/keyring/3.6.3/keyring/). Linux requires
  an unlocked Secret Service (e.g. GNOME Keyring/KWallet) and a session D-Bus.
- Browser preview keeps tokens only in memory. There is no localStorage/plaintext
  fallback. Credential-store failures are shown; the current window can continue
  without persistent storage. Store keys are scoped to the compiled backend URL.
- Logout tries server revocation and clears local credentials. If unreachable,
  the UI reports that server revocation was not confirmed.

Account APIs require HTTPS outside loopback development. Bearer headers are not
ambient cookies, so cross-origin forms cannot authenticate mutations. CORS permits
only configured origins and the Authorization/Content-Type headers. Proxy headers
are ignored for rate limiting: deploy a trusted proxy policy before using forwarded
client addresses. As written, proxied clients share the proxy IP's rate budget.

Argon2id uses the RustCrypto default v19 parameters (19 MiB, two iterations, one
lane), random salts and a maximum of four concurrent account blocking workers.
PostgreSQL rate counters survive restarts: 180 account requests/IP/minute, 30
sensitive operations/IP/minute, and six registration/login/recovery attempts per
normalized identifier/minute. Limits apply to successes and failures; HTTP 429
includes `Retry-After: 60`. Request bodies are limited to 16 KiB. Tokens and
password request types deliberately lack `Debug` implementations.

## API and checks

`POST /api/v1/account` takes the tagged `AccountRequest` from
`shared/src/account.rs`, such as `{"action":"current"}`, with
`Authorization: Bearer <token>` for authenticated actions. `AccountResponse` has a
`result` tag; failures use the shared `ApiError` with request ID. Responses are
`Cache-Control: no-store`. OAuth callback queries are excluded from request logs.

Run backend/shared tests in WSL with `--include-ignored` and a dedicated
`TEST_DATABASE_URL`. They create/drop only random schemas. Tests cover real
PostgreSQL migrations, credentials, verification/reset replay, session rotation,
expiry/revocation, reauthentication, throttling, deletion, callback cancellation
and OAuth failures. Signed offline OIDC fixtures exercise token validation;
the RSA fixture key is deliberately public and never used by the application.

Native callback tests run in the desktop CI matrix. The optional
`credential_store_round_trip` test needs an unlocked OS store and uses a separate
test-only service entry, removed afterwards. Resend request construction and provider
failures are tested against a local mock, without sending real emails. Google consent,
live Resend/SMTP delivery and
desktop browser/credential prompts on each OS still need live acceptance checks
with the configured providers. Compilation does not establish macOS runtime behavior.

## Session checks and activity

Routine authorization rechecks the account and session under compatible `FOR SHARE`
locks for the action transaction, in account-then-session order. Credential/identity
changes, rotation, deletion and revocation retain exclusive account/session locking.
Token activity is read after the session lock is acquired so a concurrent rotation
cannot authorize a previously active token. Replay revocation still commits in its
own transaction even when the requesting action fails.

Successful authentication refreshes `last_seen_at` at most once per five minutes,
using an exclusive session lock only on that slow path (or for replay revocation).
Recent checks do not write activity timestamps. The seven-day idle cutoff therefore
uses coalesced activity and can expire a session up to five minutes earlier than
its last request; the absolute expiry is unchanged. Expired/revoked sessions are
never revived by an activity refresh.
