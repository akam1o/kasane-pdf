#!/usr/bin/env bash
# Run one fuzz target under the CI limits (pr.yml render-fuzz-smoke,
# fuzz-nightly.yml) and fail on anything libFuzzer reports — not only on
# its exit status: a file left in fuzz/artifacts/<target>/ (crash-,
# timeout-, oom-, leak-, slow-unit-), an error marker in the log, or a
# run that never printed its final "Done N runs" line fails too.
#
#   fuzz/ci-run.sh TARGET CORPUS_DIR... -- [extra libFuzzer flags]
#
# libFuzzer writes the new inputs it keeps into the first CORPUS_DIR. Run
# locally with a scratch directory first, so the committed seeds stay as
# they are:  fuzz/ci-run.sh TARGET "$(mktemp -d)" fuzz/seeds/TARGET -- ...
#
# Limits: 2 GiB RSS and malloc, 30 s per input, 90 s for render_content
# ($FUZZ_TIMEOUT overrides both; an input slower than that is a timeout,
# and libFuzzer's slow-unit report uses the same bound); the caller adds
# the run count or -max_total_time.
# Toolchain: $FUZZ_TOOLCHAIN (CI's pinned nightly by default).
#
# Why 30 s: a render target renders each input up to five times (two
# passes, the equivalence variants) and extracts its text as often, under
# ASan (≈ 9×
# slower) on a runner ≈ 5× slower than a workstation. The renderer's own
# bounds allow ≈ 70 ms for one render of render_content's page (e.g.
# 500 000 butt-capped dash segments along a million-unit path), which
# that multiplies to ≈ 14 s on the runner: a 10 s limit fired on
# legitimate inputs (fuzz-nightly, PR #3).
#
# Why 90 s for render_content: that estimate is one dashed path. A page
# of two million-unit dashed rectangles rendered in ≈ 210 ms natively,
# five times per input: ≈ 8 s per execution under ASan on a workstation
# and 31–35 s on the slowest runners, so 30 s fired on legitimate inputs
# twice in three nightly runs on main (2026-11-16). Since D1 the
# renderer bounds a render's dashed outline edges together (4 × 10⁶):
# that page is refused after its first rectangle (≈ 3 s per execution),
# but a page just within the budget still takes ≈ 10 s per execution on
# a workstation, ≈ 21 s on such a runner — too near 30 s to go back. A
# hang still fails, at the nominal 90 s (libFuzzer checks the bound
# every timeout/2 + 1 s, so up to about 136 s).
set -uo pipefail
# Coverage-only mode (the cmin step's) runs no oracle: never fuzz with it.
if [ -n "${KASANE_FUZZ_COVERAGE_ONLY+x}" ]; then
  echo "::error::KASANE_FUZZ_COVERAGE_ONLY is set: the targets would run without their oracles"
  exit 1
fi
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
case $target in
  render_content) default_timeout=90 ;;
  *) default_timeout=30 ;;
esac
timeout=${FUZZ_TIMEOUT:-$default_timeout}
log=$(mktemp)
cargo "+$toolchain" fuzz run -O "$target" "${dirs[@]}" -- \
  -rss_limit_mb=2048 -malloc_limit_mb=2048 -timeout="$timeout" -report_slow_units="$timeout" \
  -print_final_stats=1 "$@" 2>&1 | tee "$log"
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
