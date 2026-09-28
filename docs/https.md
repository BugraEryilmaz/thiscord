# Direct HTTPS in WSL

The backend can terminate TLS itself using Rustls. A reverse proxy is optional.
Set these in the ignored `backend/.env` for the existing certificate:

```dotenv
BACKEND_BIND=0.0.0.0:443
TLS_CERT_PATH=/etc/letsencrypt/live/thiscord.com.tr/fullchain.pem
TLS_KEY_PATH=/etc/letsencrypt/live/thiscord.com.tr/privkey.pem
PUBLIC_BACKEND_URL=https://thiscord.com.tr
GOOGLE_REDIRECT_URL=https://thiscord.com.tr/api/v1/account/google/callback
```

Keep credentials on separate lines. Preserve the existing database, Resend and
Google credentials. Include `https://thiscord.com.tr` in `ALLOWED_ORIGINS` alongside
the desktop origins. The certificate must cover the exact hostname clients use;
the current certificate covers `thiscord.com.tr`, not `api.thiscord.com.tr`.

Register the exact HTTPS callback above in the Google OAuth web client's
authorized redirect URIs. Changing `.env` does not update Google's console.
Verification and Google login reject plain HTTP public URLs; this previously
produced account HTTP 503 responses before contacting the provider.

Run `./run-wsl.ps1` from `backend/`. The startup log reports `scheme="https"`.
The application user needs read access to both PEM files; keep the private key
restricted and outside the repository. It also needs permission to bind port 443.
Both TLS variables unset retains local HTTP development; setting only one, or
using unreadable/invalid/mismatched PEM files, fails startup without HTTP fallback.
Database-only CLI operations do not require certificate access.

After Certbot successfully renews the files, send `SIGHUP` to the running backend
PID (`kill -HUP PID`) or restart it. Integrate that action into the deployment's
Certbot deploy hook. SIGHUP reloads new handshakes without disconnecting current
clients; a failed reload retains the previous certificate and logs an error.
This code loads certificates; certificate issuance and renewal remain Certbot's job.

Forward public TCP port 443 through the router and Windows firewall/networking
to WSL port 443. Windows TCP portproxy entries, if used, must target the current
WSL IP, which can change after restarting WSL. Test from a different network as
well as locally. The TLS listener serves the backend API; it does not serve the
frontend website or redirect HTTP port 80. Stop any older plaintext backend before
using production accounts. Keep PostgreSQL private.

Build clients with the HTTPS origin in **both** WASM and native builds:

```powershell
cd C:\thiscord\frontend
$env:THISCORD_API_URL = "https://thiscord.com.tr"
cargo tauri build
```

For development use the same variable with `cargo tauri dev`. Existing clients
compiled for HTTP need rebuilding. Both Tauri CSPs allow HTTPS/WSS to this hostname.
Changing the backend origin uses a separate OS credential-store entry, so sign in
again. HTTPS also protects text and voice WebSocket signaling; it does not replace
WebRTC UDP reachability or STUN/TURN. See [audio.md](audio.md).

Verify the backend with `https://thiscord.com.tr/api/v1/health` and
`https://thiscord.com.tr/api/v1/ready`. HTTPS tests use ephemeral self-signed fixtures,
check hostname verification, plaintext rejection, peer addresses and failed reloads.
