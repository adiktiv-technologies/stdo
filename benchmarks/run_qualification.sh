#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

root=$(cd "$(dirname "$0")/.." && pwd)
stdo=${STDO_BIN:-"$root/target/release/stdo"}
runs=${RUNS:-15}
warmup=${WARMUP:-2}
[[ "$stdo" = /* ]] || stdo="$PWD/$stdo"
[[ "$runs" =~ ^[1-9][0-9]*$ && "$warmup" =~ ^[1-9][0-9]*$ ]] || {
  printf 'RUNS and WARMUP must be positive integers.\n' >&2
  exit 2
}
[[ -x "$stdo" ]] || {
  printf 'Build a release binary first or set STDO_BIN.\n' >&2
  exit 2
}
command -v jq >/dev/null

workspace=$(mktemp -d "${TMPDIR:-/tmp}/stdo-qualification.XXXXXXXX")
trap 'rm -rf -- "$workspace"' EXIT
cd "$workspace"

cat > pipeline.yaml <<'YAML'
version: '1.0'
name: qualification
steps:
  - id: alpha
    cmd: [sh, -c, 'sleep 0.04; printf "%s\n" "$1"', worker, alpha]
    cache: true
  - id: beta
    cmd: [sh, -c, 'sleep 0.04; printf "%s\n" "$1"', worker, beta]
    cache: true
  - id: gamma
    cmd: [sh, -c, 'sleep 0.04; printf "%s\n" "$1"', worker, gamma]
    cache: true
  - id: fan_in
    cmd: [sh, -c, 'cat; sleep 0.04; printf "tail\n"']
    stdin: '{{ steps.alpha.stdout }}{{ steps.beta.stdout }}{{ steps.gamma.stdout }}'
    capture_stdout: false
  - id: count
    cmd: [sh, -c, 'IFS= read -r first; printf "%s:" "$first"; wc -l']
    pipe_from: fan_in
output: '{{ steps.count.stdout | replace(" ", "") | trim }}'
YAML

worker() { sh -c 'sleep 0.04; printf "%s\n" "$1"' worker "$1"; }

bash_trial() {
  local started first finished alpha beta gamma result
  started=$(jq -n now)
  alpha=$(worker alpha)
  first=$(jq -n now)
  beta=$(worker beta)
  gamma=$(worker gamma)
  printf '%s\n' "$alpha" "$beta" "$gamma" | sh -c 'cat; sleep 0.04; printf "tail\n"' > fan-in.txt
  result=$(sh -c 'IFS= read -r first; printf "%s:" "$first"; wc -l' < fan-in.txt | tr -d ' ')
  [[ "$result" == alpha:3 ]]
  finished=$(jq -n now)
  awk -v start="$started" -v first="$first" -v end="$finished" \
    'BEGIN { printf "%.6f\t%.6f\n", (end-start)*1000, (first-start)*1000 }'
}

i=0
while (( i < warmup )); do
  bash_trial >/dev/null
  i=$((i + 1))
done
i=0
while (( i < runs )); do
  bash_trial >> bash-samples.tsv
  i=$((i + 1))
done

[[ $("$stdo" run -f pipeline.yaml --no-cache) == alpha:3 ]]
"$stdo" bench pipeline.yaml --runs "$runs" --warmup "$warmup" --no-cache --report cold.json >/dev/null
"$stdo" bench pipeline.yaml --runs "$runs" --warmup "$warmup" --report warm.json >/dev/null
jq -e '.workflows[0].cache_hit_ratio == 0.6' warm.json >/dev/null
[[ $("$stdo" run -f pipeline.yaml) == alpha:3 ]]

printf '%s | %s | %s runs, %s warmups\n' "$(uname -sm)" "$("$stdo" --version)" "$runs" "$warmup"
printf '| Mode | Wall p50 (ms) | Wall p95 (ms) | Mean TTFB (ms) | Peak monitored bytes | Cache hits |\n'
printf '| --- | ---: | ---: | ---: | ---: | ---: |\n'
sort -n -k1,1 bash-samples.tsv | awk '
  function percentile(q, x, i, j) {
    x = 1 + (NR - 1) * q
    i = int(x)
    j = i < NR ? i + 1 : i
    return latency[i] + (latency[j] - latency[i]) * (x - i)
  }
  { latency[NR] = $1; first += $2 }
  END {
    printf "| Sequential Bash | %.2f | %.2f | %.2f | n/a | n/a |\n", percentile(0.5), percentile(0.95), first/NR
  }
'
for mode in cold warm; do
  jq -r --arg mode "$mode" '.workflows[0] |
    def round2: (. * 100 | round) / 100;
    "| stdo \($mode) | \(.latency.p50_ms | round2) | \(.latency.p95_ms | round2) | \(.ttfb_mean_ms | round2) | \(.peak_bytes_max) | \(.cache_hit_ratio * 100)% |"' "$mode.json"
done
