#!/usr/bin/env bash
# Replay a target's seeds and fuzz new inputs from them, in parallel
# shards (pr.yml render-fuzz-smoke, redact_model):
#
#   fuzz/ci-run-sharded.sh TARGET SEED_DIR SHARDS NEW_PER_SHARD MAX_SECONDS [FIRST LAST]
#
# The seeds (every regular file of SEED_DIR, in byte order) are dealt
# round-robin into SHARDS directories, and shards FIRST..LAST (default:
# all) run here, in parallel — several jobs can split one target's
# shards between them; each shard runs fuzz/ci-run.sh (which decides
# failure: exit status, saved inputs, error markers, the final "Done"
# line) with libFuzzer seed k. libFuzzer first executes every seed of
# its shard — the replay, never cut short by -max_total_time — and its
# -runs count includes them, so -runs = seeds + 1 (the empty input) +
# NEW_PER_SHARD fuzzes NEW_PER_SHARD new inputs from that shard's
# seeds, or as many as MAX_SECONDS allows. Any failing shard fails the
# run; so does a setup problem (no seeds, a shard left empty, a bad
# argument, a copy that failed). $FUZZ_CI_RUN replaces fuzz/ci-run.sh
# (tests).
set -euo pipefail
die() { echo "::error::ci-run-sharded: $*"; exit 1; }
[ $# -eq 5 ] || [ $# -eq 7 ] || die "usage: TARGET SEED_DIR SHARDS NEW_PER_SHARD MAX_SECONDS [FIRST LAST]"
target=$1 seeds=$2 shards=$3 new=$4 max=$5 first=${6:-1} last=${7:-$3}
for v in "$shards" "$new" "$max" "$first" "$last"; do
  [[ $v =~ ^[0-9]+$ ]] || die "not a number: $v"
done
[ "$shards" -ge 1 ] && [ "$max" -ge 1 ] || die "SHARDS and MAX_SECONDS must be positive"
[ "$first" -ge 1 ] && [ "$first" -le "$last" ] && [ "$last" -le "$shards" ] \
  || die "shards $first-$last are not within 1-$shards"
[ -d "$seeds" ] || die "no seed directory $seeds"
here=$(cd "$(dirname "$0")" && pwd)
run=${FUZZ_CI_RUN:-$here/ci-run.sh}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
for k in $(seq 1 "$shards"); do mkdir "$work/shard-$k"; done
i=0
while IFS= read -r -d '' f; do
  k=$((i % shards + 1))
  cp -- "$f" "$work/shard-$k/"
  i=$((i + 1))
done < <(find "$seeds" -maxdepth 1 -type f -print0 | LC_ALL=C sort -z)
[ "$i" -ge "$shards" ] || die "$i seed(s) for $shards shards: a shard would replay nothing"
echo "ci-run-sharded: $target, $i seeds in $shards shards; shards $first-$last here, $new new inputs each (at most $max s)"
pids=()
for k in $(seq "$first" "$last"); do
  n=$(find "$work/shard-$k" -type f | wc -l | tr -d ' ')
  "$run" "$target" "$work/shard-$k" -- \
    -seed="$k" -runs=$((n + 1 + new)) -max_total_time="$max" > "$work/log-$k" 2>&1 &
  pids+=($!)
done
failed=0
for k in $(seq "$first" "$last"); do
  if ! wait "${pids[$((k - first))]}"; then
    failed=$((failed + 1))
  fi
  echo "--- shard $k"
  grep -E '^(ci-run:|::error::|  artifact:)|INITED|^Done' "$work/log-$k" || echo "(no runner summary)"
done
if [ "$failed" -ne 0 ]; then
  die "$failed of shards $first-$last of $target failed"
fi
echo "ci-run-sharded: $target passed (shards $first-$last of $shards)"
