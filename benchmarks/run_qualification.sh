#!/usr/bin/env bash
set -euo pipefail
export LC_ALL=C

root=$(cd "$(dirname "$0")/.." && pwd)
script="$root/benchmarks/run_qualification.sh"

bash_worker() {
  local name=$1 stamp=$2
  sh -c 'sleep 0.04; printf "%s\n" "$1"' worker "$name" > "$name.out" || return
  if [[ "$stamp" == yes ]]; then
    jq -n now > "$name.time"
  fi
}

bash_pipeline() {
  local mode=$1 stamp=${2:-no} name pid failed=0
  local pids=()
  for name in alpha beta gamma; do
    if [[ "$mode" == parallel ]]; then
      bash_worker "$name" "$stamp" &
      pids+=("$!")
    else
      bash_worker "$name" "$stamp"
    fi
  done
  if [[ "$mode" == parallel ]]; then
    for pid in "${pids[@]}"; do
      wait "$pid" || failed=1
    done
    (( failed == 0 )) || return 1
  fi
  cat alpha.out beta.out gamma.out |
    sh -c 'cat; sleep 0.04; printf "tail\n"' |
    sh -c 'IFS= read -r first; printf "%s:" "$first"; wc -l' |
    tr -d ' '
}

workspace=$(mktemp -d "${TMPDIR:-/tmp}/stdo-qualification.XXXXXXXX")
trap 'rm -rf -- "$workspace"' EXIT

if [[ "${1:-}" == --bash-baseline && ( "${2:-}" == sequential || "${2:-}" == parallel ) ]]; then
  cd "$workspace"
  bash_pipeline "$2"
  exit
fi

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

(cd "$root" && [[ $("$stdo" run -f benchmarks/parallel-bash-baseline.yaml) == alpha:3 ]])
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

bash_trial() {
  local mode=$1 started
  local TIMEFORMAT='%3R'
  # Measure wall time without timestamp subprocesses on the pipeline's path.
  { time bash_pipeline "$mode" > bash-output.txt 2>&3; } 3>&2 2> bash-wall.txt
  [[ $(<bash-output.txt) == alpha:3 ]]

  # Time to first worker output is measured in a separate execution.
  started=$(jq -n now)
  bash_pipeline "$mode" yes > bash-output.txt
  [[ $(<bash-output.txt) == alpha:3 ]]
  jq -rs --argjson start "$started" --argjson wall "$(<bash-wall.txt)" \
    '[$wall * 1000, (min - $start) * 1000] | @tsv' alpha.time beta.time gamma.time
}

for mode in sequential parallel; do
  for ((i = 0; i < warmup; i++)); do
    bash_trial "$mode" >/dev/null
  done
  for ((i = 0; i < runs; i++)); do
    bash_trial "$mode" >> "$mode.tsv"
  done
done

[[ $("$stdo" run -f pipeline.yaml --no-cache) == alpha:3 ]]
"$stdo" bench pipeline.yaml --runs "$runs" --warmup "$warmup" --no-cache --report cold.json >/dev/null
"$stdo" bench pipeline.yaml --runs "$runs" --warmup "$warmup" --report warm.json >/dev/null
jq -e '.workflows[0].cache_hit_ratio == 0.6' warm.json >/dev/null
[[ $("$stdo" run -f pipeline.yaml) == alpha:3 ]]

rss_sample() {
  local destination=$1
  shift
  case "$os" in
    Darwin) /usr/bin/time -l "$@" 2> rss-report.txt || return ;;
    Linux) /usr/bin/time -v "$@" 2> rss-report.txt || return ;;
    *) return 1 ;;
  esac
  awk -v os="$os" '
    os == "Darwin" && /maximum resident set size/ { print int(($1 + 1023) / 1024); found = 1 }
    os == "Linux" && /Maximum resident set size \(kbytes\):/ { print $NF; found = 1 }
    END { if (!found) exit 1 }
  ' rss-report.txt >> "$destination"
}

rss_max() {
  if [[ -s "$1.rss" ]]; then
    awk '$1 > max { max = $1 } END { print max + 0 }' "$1.rss"
  else
    printf 'n/a'
  fi
}

os=$(uname -s)
if rss_sample probe.rss /usr/bin/true; then
  for ((i = 0; i < runs; i++)); do
    rss_sample sequential.rss bash "$script" --bash-baseline sequential > rss-output.txt
    [[ $(<rss-output.txt) == alpha:3 ]]
    rss_sample parallel.rss bash "$script" --bash-baseline parallel > rss-output.txt
    [[ $(<rss-output.txt) == alpha:3 ]]
    rss_sample cold.rss "$stdo" run -f pipeline.yaml --no-cache > rss-output.txt
    [[ $(<rss-output.txt) == alpha:3 ]]
    rss_sample warm.rss "$stdo" run -f pipeline.yaml > rss-output.txt
    [[ $(<rss-output.txt) == alpha:3 ]]
  done
else
  printf 'OS RSS unavailable from /usr/bin/time on %s.\n' "$os" >&2
  if [[ -s rss-report.txt ]]; then cat rss-report.txt >&2; fi
fi

printf '%s | %s | %s runs, %s warmups\n' "$(uname -sm)" "$("$stdo" --version)" "$runs" "$warmup"
printf '| Mode | Wall p50 (ms) | Wall p95 (ms) | Mean TTFB (ms) | Peak monitored bytes | OS max RSS (KiB) | Cache hits |\n'
printf '| --- | ---: | ---: | ---: | ---: | ---: | ---: |\n'
for mode in sequential parallel; do
  if [[ "$mode" == sequential ]]; then label='Sequential Bash'; else label='Parallel Bash'; fi
  sort -n -k1,1 "$mode.tsv" | awk -v label="$label" -v rss="$(rss_max "$mode")" '
  function percentile(q, x, i, j) {
    x = 1 + (NR - 1) * q
    i = int(x)
    j = i < NR ? i + 1 : i
    return latency[i] + (latency[j] - latency[i]) * (x - i)
  }
  { latency[NR] = $1; first += $2 }
  END {
    printf "| %s | %.2f | %.2f | %.2f | n/a | %s | n/a |\n", label, percentile(0.5), percentile(0.95), first/NR, rss
  }
'
done
for mode in cold warm; do
  jq -r --arg mode "$mode" --arg rss "$(rss_max "$mode")" '.workflows[0] |
    def round2: (. * 100 | round) / 100;
    "| stdo \($mode) | \(.latency.p50_ms | round2) | \(.latency.p95_ms | round2) | \(.ttfb_mean_ms | round2) | \(.peak_bytes_max) | \($rss) | \(.cache_hit_ratio * 100)% |"' "$mode.json"
done
