#!/usr/bin/env bash
# Finds a scenario's sustainable rate on THIS machine (D-102): steps the offered rate up
# through MODE=full runs until the run's own thresholds fail — refused share over 1% or an
# op's latency over its limit — and reports the knee: the last rate that carried its shape.
#
#   scripts/load-capacity.sh <scenario> [start_rps] [step_secs] [max_steps]
#
# A full run above the knee can only measure shedding — evidence is a run inside capacity.
# The knee record (perf/results/<stamp>-<scenario>-capacity.json) carries every step, so
# "run at 60% of the knee" is a number, not a hope, and the machine class rides along in
# each step's own machine record. Needs the same env as load.sh (scripts/stack.sh env).
set -euo pipefail
cd "$(dirname "$0")/.."

scenario="${1:?usage: scripts/load-capacity.sh <scenario> [start_rps] [step_secs] [max_steps]}"
rate="${2:-100}"
step_secs="${3:-45}"
max_steps="${4:-8}"
# Probing steps use a small seed — the dataset only needs to be realistic, not full-size;
# each step re-seeds through load.sh anyway.
export MODE=full SECONDS="$step_secs" SEED="${SEED:-1500}"

steps="[]"
knee=null
aborted=""
before="$(ls -t perf/results/*-${scenario}-full.json 2>/dev/null | head -1 || true)"
for i in $(seq 1 "$max_steps"); do
  echo "capacity: step $i at ${rate} req/s (${step_secs}s)" >&2
  log="$(mktemp -t capacity-step)"
  RATE="$rate" bash scripts/load.sh "$scenario" >"$log" 2>&1 || true
  tail -15 "$log" >&2
  latest="$(ls -t perf/results/*-${scenario}-full.json 2>/dev/null | head -1 || true)"
  if [ -z "$latest" ] || [ "$latest" = "$before" ]; then
    # An aborted step is evidence, not silence: the knee record below carries it and the
    # run's own reason (a seed that never settled, a refused seed, a missing file).
    reason="$(grep -oE '(GoError: |seed [a-z]+: |fail[a-z]*[: ]).+' "$log" | tail -1 || true)"
    reason="${reason:-k6 produced no result file}"
    echo "capacity: step $i aborted: $reason" >&2
    steps="$(jq -c --argjson rate "$rate" --arg reason "$reason" \
      '. + [{rate: $rate, aborted: true, abort_reason: $reason}]' <<<"$steps")"
    aborted="$reason"
    rm -f "$log"
    break
  fi
  rm -f "$log"
  before="$latest"
  step="$(jq -c --argjson rate "$rate" '{
      rate: $rate,
      requests: (.requests // 0),
      thresholds_passed: .thresholds_passed,
      refused_share: (([.k6_summary | to_entries[] | select(.key | test("op_status\\{.*status:(503|0)")) | .value.count] | add // 0)
                     / ([.k6_summary | to_entries[] | select(.key | test("op_status\\{")) | .value.count] | add // 1)),
      machine: .machine
    }' "$latest")"
  steps="$(jq -c --argjson step "$step" '. + [$step]' <<<"$steps")"
  passed="$(jq -r '.thresholds_passed' <<<"$step")"
  echo "capacity: ${rate}/s -> passed=${passed} refused=$(jq -r '.refused_share' <<<"$step")" >&2
  if [ "$passed" = "true" ]; then
    knee="$rate"
    rate=$((rate + rate / 2 + 1))   # ~1.5x: fine enough to place the knee coarsely
  else
    break
  fi
done

stamp="$(date -u +%Y%m%dT%H%M%SZ)"
out="perf/results/${stamp}-${scenario}-capacity.json"
jq -n --arg scenario "$scenario" --arg commit "$(git rev-parse HEAD)" \
      --arg aborted "$aborted" \
      --argjson steps "$steps" --argjson knee "${knee:-null}" '
  {
    scenario: $scenario,
    measured_utc: (now | todate),
    commit: $commit,
    machine: ($steps[0].machine // null),
    aborted: ($aborted != ""),
    abort_reason: (if $aborted == "" then null else $aborted end),
    knee_requests_per_second: $knee,
    recommended_full_rate: (if $knee then ($knee * 0.6 | floor) else null end),
    rule: "knee = the last step whose own thresholds passed; a full run offered above it measures only shedding. Full evidence runs go at ~60% of the knee with the limits unchanged; overload is a separately-labelled scenario where shedding is the expectation.",
    steps: $steps
  }' >"$out"
echo "capacity: knee=${knee:-none found} req/s -> $out" >&2
