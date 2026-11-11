#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

for tool in cargo node npm ffmpeg; do
  command -v "$tool" >/dev/null || { echo "Missing capture tool: $tool" >&2; exit 1; }
done

work=$(mktemp -d)
server_pid=''
cleanup() {
  if [[ -n "$server_pid" ]]; then
    kill "$server_pid" 2>/dev/null || true
    wait "$server_pid" 2>/dev/null || true
  fi
  rm -rf "$work"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cargo build --locked --release
bin_dir=$(cd "${CARGO_TARGET_DIR:-target}/release" && pwd)
export PATH="$bin_dir:$PATH"
"$bin_dir/stdo" serve -f examples/11-composable-subworkflows.yaml --port 0 >"$work/server.log" 2>&1 &
server_pid=$!
url=''
for ((attempt=0; attempt<100; attempt++)); do
  url=$(awk '/^Studio listening on http:\/\/127\.0\.0\.1:/ {print $4; exit}' "$work/server.log")
  [[ -z "$url" ]] || break
  kill -0 "$server_pid" 2>/dev/null || break
  sleep 0.1
done
if [[ -z "$url" ]]; then
  cat "$work/server.log" >&2
  echo "Studio did not start" >&2
  exit 1
fi

npm install --prefix "$work" --no-save --no-package-lock --ignore-scripts --no-audit --no-fund playwright@1.56.1
browser_args=(--only-shell chromium)
if [[ ${CI:-} == true && $(uname -s) == Linux ]]; then
  browser_args=(--with-deps "${browser_args[@]}")
fi
node "$work/node_modules/playwright/cli.js" install "${browser_args[@]}"
node scripts/capture-studio-screenshots.mjs "$url" "$work"
mkdir -p assets
for name in studio-overview-subworkflows studio-catalog-dscr studio-catalog-subworkflows; do
  mv "$work/$name.png" "assets/$name.png"
  wc -c "assets/$name.png"
done
