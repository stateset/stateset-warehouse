#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
compatible_node() {
  [[ -x "$1" ]] && "$1" -e 'const [major,minor]=process.versions.node.split(".").map(Number); process.exit(major>22 || major===22 && minor>=12 ? 0 : 1)' >/dev/null 2>&1
}
if [[ -n "${WMS_TUI_NODE:-}" ]]; then
  if compatible_node "$WMS_TUI_NODE"; then exec "$WMS_TUI_NODE" cli/warehouse-tui.mjs "$@"; fi
  echo 'WMS_TUI_NODE must point to an executable Node.js >=22.12 binary.' >&2
  exit 1
fi
task_node_path="$(command -v node || true)"
if [[ -n "$task_node_path" ]] && compatible_node "$task_node_path"; then exec "$task_node_path" cli/warehouse-tui.mjs "$@"; fi
for task_node_path in "$HOME"/.nvm/versions/node/v*/bin/node "$HOME"/.local/share/fnm/node-versions/v*/installation/bin/node; do
  if compatible_node "$task_node_path"; then exec "$task_node_path" cli/warehouse-tui.mjs "$@"; fi
done
echo 'Install Node.js >=22.12 or set WMS_TUI_NODE to its executable path.' >&2
exit 1
