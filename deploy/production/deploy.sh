#!/usr/bin/env bash
# Deploy one release tag on this host. Run from a clone of the repository:
#   sudo bash deploy/production/deploy.sh v0.3.0
# Builds the image from the exact tag, takes a verified backup first, starts
# the new container, waits for /ready and records what was deployed.
set -euo pipefail
tag="${1:?usage: deploy.sh <release tag>}"
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../.." && pwd)"
state=/var/lib/stateset-wms
mkdir -p "$state"
for file in /etc/stateset-wms/database.env /etc/stateset-wms/app.env /etc/stateset-wms/deploy.env; do
  [[ -r "$file" ]] || { echo "Missing $file (see deploy/production/README.md)" >&2; exit 1; }
done
# shellcheck source=lib.sh
source "$here/lib.sh"
WMS_DOMAIN="$(env_value /etc/stateset-wms/deploy.env WMS_DOMAIN)"

git -C "$repo" fetch --tags --quiet
git -C "$repo" -c advice.detachedHead=false checkout --quiet "refs/tags/$tag"
revision="$(git -C "$repo" rev-parse HEAD)"
[[ -z "$(git -C "$repo" status --porcelain)" ]] || { echo "Checkout has local changes; refusing to build" >&2; exit 1; }

echo "Building stateset-warehouse:$tag from $revision"
docker build --label "org.opencontainers.image.revision=$revision" -t "stateset-warehouse:$tag" "$repo"
image="$(docker image inspect --format '{{.Id}}' "stateset-warehouse:$tag")"

echo "Taking a verified backup before the change"
bash "$here/backup.sh"

previous="$(cat "$state/current-release" 2>/dev/null || true)"
echo "Starting $tag (previous: ${previous:-none})"
WMS_RELEASE="$tag" WMS_DOMAIN="$WMS_DOMAIN" docker compose -f "$here/compose.yml" up -d --wait
WMS_RELEASE="$tag" WMS_DOMAIN="$WMS_DOMAIN" docker compose -f "$here/compose.yml" ps

curl --fail --silent --show-error "https://$WMS_DOMAIN/ready" > /dev/null
echo "$tag" > "$state/current-release"
printf '%s tag=%s revision=%s image=%s previous=%s\n' "$(date -u +%FT%TZ)" "$tag" "$revision" "$image" "${previous:-none}" >> "$state/deployments.log"
echo "Deployed $tag. Run reconciliation for each warehouse before resuming work (README.md, step 6)."
