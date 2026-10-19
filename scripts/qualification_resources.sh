#!/usr/bin/env bash
# Shared by the local image harnesses. Never acts on unlabelled resources.
wms_qualification_resource_mode() {
  case "${WMS_KEEP_QUALIFICATION_RESOURCES:-0}" in
    0|1) ;;
    *) echo 'WMS_KEEP_QUALIFICATION_RESOURCES must be 0 or 1.' >&2; return 1 ;;
  esac
}

wms_qualification_finish_resources() {
  local root="$1" label="$2" run="$3" network="$4" volume="$5"
  shift 5
  local result=0 kind name owner action outcome wms_resource_index
  local keep="${WMS_KEEP_QUALIFICATION_RESOURCES:-0}"
  local -a kinds=() names=()
  wms_qualification_resource_mode || return 1
  for name in "$@"; do kinds+=(container); names+=("$name"); done
  if [[ -n "$volume" ]]; then kinds+=(volume); names+=("$volume"); fi
  kinds+=(network); names+=("$network")
  for ((wms_resource_index=0;wms_resource_index<${#names[@]};wms_resource_index++)); do
    kind="${kinds[$wms_resource_index]}"; name="${names[$wms_resource_index]}"
    if [[ "$kind" == container ]]; then
      owner="$(docker inspect --format "{{ index .Config.Labels \"$label\" }}" "$name" 2>/dev/null || true)"
    else
      owner="$(docker "$kind" inspect --format "{{ index .Labels \"$label\" }}" "$name" 2>/dev/null || true)"
    fi
    action=none
    if [[ -z "$owner" ]]; then
      outcome=not_observed
    elif [[ "$owner" != "$run" ]]; then
      outcome=ownership_mismatch
      result=1
    elif [[ "$kind" == container ]]; then
      if [[ "$keep" == 1 ]]; then
        action=stop
        if docker stop --time 15 "$name" >> "$root/cleanup.log" 2>&1; then
          outcome=retained
        else
          outcome=failed; result=1
        fi
        docker inspect --format '{{json .State}}' "$name" > "$root/$name-final-state.json" 2>> "$root/cleanup.log" || result=1
      else
        action=remove
        docker logs "$name" > "$root/$name.log" 2>&1 || true
        if docker rm --force --volumes "$name" >> "$root/cleanup.log" 2>&1; then
          outcome=removed
        else
          outcome=failed; result=1
        fi
      fi
      if [[ "$keep" == 1 ]]; then docker logs "$name" > "$root/$name.log" 2>&1 || true; fi
    elif [[ "$keep" == 1 ]]; then
      outcome=retained
    else
      action=remove
      if docker "$kind" rm "$name" >> "$root/cleanup.log" 2>&1; then
        outcome=removed
      else
        outcome=failed; result=1
      fi
    fi
    python3 - "$root/resource-disposition.jsonl" "$kind" "$name" "$run" "$owner" "$action" "$outcome" "$keep" <<'PY' || result=1
import json, pathlib, sys
path, kind, name, expected, observed, action, outcome, keep = sys.argv[1:]
with pathlib.Path(path).open("a") as stream:
    stream.write(json.dumps(dict(kind=kind, name=name, expected_owner=expected,
                                observed_owner=observed, action=action, outcome=outcome,
                                retention_requested=keep == "1")) + "\n")
PY
  done
  return "$result"
}
