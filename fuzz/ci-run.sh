#!/usr/bin/env bash
# Run one fuzz target under the CI limits (pr.yml render-fuzz-smoke,
# fuzz-nightly.yml) and fail on anything libFuzzer reports — not only on
# its exit status: a file left in fuzz/artifacts/<target>/ (crash-,
# timeout-, oom-, leak-, slow-unit-), an error marker in the log, or a
# run that never printed its final "Done N runs" line fails too.
#
#   fuzz/ci-run.sh TARGET CORPUS_DIR... -- [extra libFuzzer flags]
#
# Limits: 2 GiB RSS and malloc, 10 s per input; the caller adds the run
# count or -max_total_time. Toolchain: $FUZZ_TOOLCHAIN (CI's pinned
# nightly by default).
set -uo pipefail
target=$1
shift
# Corpus directories are relative to the caller's directory.
dirs=()
while [ $# -gt 0 ] && [ "$1" != "--" ]; do
  case $1 in
    /*) dirs+=("$1") ;;
    *) dirs+=("$PWD/$1") ;;
  esac
  shift
done
[ $# -gt 0 ] && shift
cd "$(dirname "$0")"
toolchain=${FUZZ_TOOLCHAIN:-nightly-2026-10-29}
log=$(mktemp)
cargo "+$toolchain" fuzz run -O "$target" "${dirs[@]}" -- \
  -rss_limit_mb=2048 -malloc_limit_mb=2048 -timeout=10 -print_final_stats=1 "$@" 2>&1 | tee "$log"
status=${PIPESTATUS[0]}
artifacts=$(find "artifacts/$target" -type f 2>/dev/null | wc -l | tr -d ' ')
markers=$(grep -cE '^==[0-9]+== ERROR: libFuzzer|^SUMMARY: libFuzzer|^ERROR: libFuzzer|panicked at' "$log")
finished=$(grep -cE '^Done [0-9]+ runs' "$log")
peak=$(grep -oE 'stat::peak_rss_mb:[[:space:]]+[0-9]+' "$log" | grep -oE '[0-9]+$' | tail -1)
runs=$(grep -oE '^Done [0-9]+ runs in [0-9]+ second' "$log" | tail -1)
rm -f "$log"
echo "ci-run: $target status=$status artifacts=$artifacts error-markers=$markers finished=$finished peak_rss_mb=${peak:-?} ${runs:-}"
if [ "$status" -ne 0 ] || [ "$artifacts" -ne 0 ] || [ "$markers" -ne 0 ] || [ "$finished" -eq 0 ]; then
  echo "::error::fuzz target $target failed (status $status, $artifacts artifact(s), $markers error marker(s), finished $finished)"
  find "artifacts/$target" -type f 2>/dev/null | sed 's/^/  artifact: /'
  exit 1
fi
