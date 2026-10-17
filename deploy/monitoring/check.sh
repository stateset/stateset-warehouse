#!/usr/bin/env bash
# Validate operational policies with the exact test tool, without network access
# or writes to the repository from inside the container.
set -euo pipefail
cd "$(dirname "$0")/../.."
wms_rules_dir="$PWD/deploy/monitoring"
wms_promtool_image='prom/prometheus@sha256:565ee86501224ebbb98fc10b332fa54440b100469924003359edf49cbce374bd'
for wms_action in version check test; do
  case "$wms_action" in
    version) wms_arguments=(--version) ;;
    check) wms_arguments=(check rules /rules/alerts.yml) ;;
    test) wms_arguments=(test rules /rules/alerts.test.yml) ;;
  esac
  docker run --rm --network none --read-only \
    --tmpfs /tmp:rw,nosuid,nodev,noexec,size=64m \
    --cap-drop ALL --security-opt no-new-privileges \
    --mount "type=bind,src=$wms_rules_dir,dst=/rules,readonly" \
    --entrypoint /bin/promtool "$wms_promtool_image" "${wms_arguments[@]}"
done
