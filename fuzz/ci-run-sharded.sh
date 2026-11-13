#!/usr/bin/env bash
# Replay a target's seeds and fuzz new inputs from them, in parallel
# shards (pr.yml render-fuzz-smoke, redact_model):
#
#   fuzz/ci-run-sharded.sh TARGET SEED_DIR SHARDS NEW_PER_SHARD MAX_SECONDS [FIRST LAST]
#
# The seeds are dealt round-robin into SHARDS directories, and shards
# FIRST..LAST (default: all) run here, in parallel — several jobs can
# split one target's shards between them; each shard
# runs fuzz/ci-run.sh (which decides failure: exit status, saved
# inputs, error markers, the final "Done" line) with libFuzzer seed
# k = 1..SHARDS. libFuzzer first executes every seed of its shard — the
# replay, never cut short by -max_total_time — and its -runs count
# includes them, so -runs = seeds + 1 (the empty input) + NEW_PER_SHARD
# fuzzes NEW_PER_SHARD new inputs from that shard's seeds, or as many as
# MAX_SECONDS allows. Any failing shard fails the run.
set -uo pipefail
target=$1 seeds=$2 shards=$3 new=$4 max=$5 first=${6:-1} last=${7:-$3}
here=$(cd "$(dirname "$0")" && pwd)
[ -d "$seeds" ] || { echo "::error::no seed directory $seeds"; exit 1; }
work=$(mktemp -d)
i=0
for f in $(cd "$seeds" && ls | sort); do
  k=$((i % shards + 1))
  mkdir -p "$work/shard-$k"
  cp "$seeds/$f" "$work/shard-$k/"
  i=$((i + 1))
done
echo "ci-run-sharded: $target, $i seeds in $shards shards; shards $first-$last here, $new new inputs each (at most $max s)"
pids=()
for k in $(seq "$first" "$last"); do
  mkdir -p "$work/shard-$k"
  n=$(ls "$work/shard-$k" | wc -l | tr -d ' ')
  "$here/ci-run.sh" "$target" "$work/shard-$k" -- \
    -seed="$k" -runs=$((n + 1 + new)) -max_total_time="$max" > "$work/log-$k" 2>&1 &
  pids+=($!)
done
failed=0
for k in $(seq "$first" "$last"); do
  wait "${pids[$((k - first))]}" || failed=$((failed + 1))
  echo "--- shard $k"
  grep -E '^(ci-run:|::error::|  artifact:)|INITED|^Done' "$work/log-$k"
done
rm -rf "$work"
if [ "$failed" -ne 0 ]; then
  echo "::error::$failed of shards $first-$last of $target failed"
  exit 1
fi
echo "ci-run-sharded: $target passed (shards $first-$last of $shards)"
