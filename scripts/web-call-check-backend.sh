#!/bin/bash
# Isolated local backend for scripts/web-call-check.py: its own database and role,
# file mail, plain HTTP on localhost (a secure context in Chromium).
# WSL/Linux only; needs passwordless sudo for the postgres role. Build the bundle
# first: (cd frontend && trunk build --release --locked --public-url /app/ --dist dist-app).
# Never point this at a real database or the production `backend/.env`.
set -u
cat > /tmp/thiscord-webcheck-db.sql <<'SQL'
DO $do$ BEGIN
  IF NOT EXISTS (SELECT FROM pg_roles WHERE rolname = 'thiscord_webcheck') THEN
    CREATE ROLE thiscord_webcheck LOGIN PASSWORD 'webcheck-local-only';
  END IF;
END $do$;
SQL
sudo -n -u postgres psql -q -f /tmp/thiscord-webcheck-db.sql 2>&1 | grep -v collation | grep -v DETAIL | grep -v HINT
sudo -n -u postgres psql -Atc "SELECT 1 FROM pg_database WHERE datname='thiscord_webcheck'" 2>/dev/null | grep -q 1 \
  || sudo -n -u postgres psql -q -c "CREATE DATABASE thiscord_webcheck OWNER thiscord_webcheck TEMPLATE template0"
export DATABASE_URL="postgres://thiscord_webcheck:webcheck-local-only@127.0.0.1/thiscord_webcheck"
PORT=${WEBCHECK_PORT:-3900}
export BACKEND_BIND=127.0.0.1:$PORT
# WEBCHECK_EXTRA_ORIGIN adds one origin, e.g. http://tauri.localhost for a local desktop build.
export ALLOWED_ORIGINS=http://localhost:$PORT${WEBCHECK_EXTRA_ORIGIN:+,$WEBCHECK_EXTRA_ORIGIN}
export PUBLIC_BACKEND_URL=http://localhost:$PORT
export MAIL_MODE=file
export MAIL_DIRECTORY=/tmp/hc-mail
export RUST_LOG=info
export APP_DIST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)/frontend/dist-app"
export THISCORD_VOICE_BOT_PATH=$HOME/.cache/thiscord-target/debug/thiscord-voice-bot
mkdir -p /tmp/hc-mail; chmod 700 /tmp/hc-mail
BIN=$HOME/.cache/thiscord-target/debug/thiscord-backend
if [ "${1:-}" = "--bootstrap-owner" ]; then exec $BIN --bootstrap-owner "$2"; fi
exec $BIN
