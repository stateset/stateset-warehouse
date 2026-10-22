#!/usr/bin/env bash
# Logical backup of the WMS database in pg_dump custom format, with a
# SHA-256 and a structural check of the archive. Keeps WMS_BACKUP_KEEP days.
# Run daily from the systemd timer and before every deployment.
set -euo pipefail
# shellcheck source=lib.sh
source "$(dirname "$0")/lib.sh"
DATABASE_URL="$(env_value /etc/stateset-wms/database.env DATABASE_URL)"
export DATABASE_URL
dir="${WMS_BACKUP_DIR:-/var/backups/stateset-wms}"
keep="${WMS_BACKUP_KEEP:-14}"
image="${WMS_PG_CLIENT_IMAGE:-postgres:16-alpine}"
mkdir -p "$dir"; chmod 700 "$dir"
stamp="$(date -u +%Y%m%dT%H%M%SZ)"
file="$dir/stateset-wms-$stamp.dump"
# Use a pinned PostgreSQL 16 client so dumps match the server version.
docker run --rm --network host -e DATABASE_URL -v "$dir:/backup" "$image" \
  sh -c "pg_dump --format=custom --no-owner --file=/backup/$(basename "$file") \"\$DATABASE_URL\""
docker run --rm -v "$dir:/backup:ro" "$image" pg_restore --list "/backup/$(basename "$file")" > /dev/null
sha256sum "$file" > "$file.sha256"
find "$dir" -name 'stateset-wms-*.dump*' -mtime "+$keep" -delete
echo "Backup $file ($(du -h "$file" | cut -f1)) verified"
