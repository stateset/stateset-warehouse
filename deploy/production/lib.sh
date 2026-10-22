# shellcheck shell=bash
# Read KEY=VALUE from an env file literally. Values are never evaluated by
# the shell, so URLs with & or ? and JSON stay intact.
env_value() {
  local file="$1" key="$2" line
  line="$(grep -m1 -E "^${key}=" "$file" || true)"
  [[ -n "$line" ]] || { echo "Missing $key in $file" >&2; return 1; }
  printf '%s' "${line#*=}"
}
