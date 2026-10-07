#!/bin/sh
set -eu
umask 077
directory=/var/backups/thiscord
install -d -m 0700 "$directory"
archive="$directory/thiscord-$(date -u +%Y%m%dT%H%M%SZ).dump"
trap 'rm -f -- "$archive.partial"' EXIT
runuser -u postgres -- pg_dump --format=custom --no-owner --no-acl thiscord > "$archive.partial"
pg_restore --list "$archive.partial" >/dev/null
mv "$archive.partial" "$archive"
# Retain daily snapshots for 14 days. The cutover archive has a different name.
find "$directory" -maxdepth 1 -type f -name 'thiscord-*.dump' -mtime +14 -delete
