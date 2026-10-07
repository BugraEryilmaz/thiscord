#!/bin/sh
set -eu
# Certbot deploy hook; ignore certificates belonging to other applications.
[ "${RENEWED_LINEAGE:-}" = /etc/letsencrypt/live/thiscord.com.tr ] || exit 0
install -o root -g thiscord -m 0640 "$RENEWED_LINEAGE/fullchain.pem" /etc/thiscord/tls/fullchain.pem.new
install -o root -g thiscord -m 0640 "$RENEWED_LINEAGE/privkey.pem" /etc/thiscord/tls/privkey.pem.new
mv /etc/thiscord/tls/fullchain.pem.new /etc/thiscord/tls/fullchain.pem
mv /etc/thiscord/tls/privkey.pem.new /etc/thiscord/tls/privkey.pem
if systemctl is-active --quiet thiscord.service; then
    systemctl reload thiscord.service
fi
