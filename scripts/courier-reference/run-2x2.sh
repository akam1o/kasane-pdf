#!/bin/sh
# One cell of the 2×2 Courier evaluation (docs/courier-reference.md): the
# nightly's corpus run and its second-witness step for one implementation
# in one reference environment.
#
#   scripts/courier-reference/run-2x2.sh SRC CORPUS OUT e0|e1 [E1DIR]
#
# SRC: a checkout of the implementation (A or C), built here as the
# nightly builds it (runner and CLI with cjk-fallback). e1 puts E1DIR/bin
# — Poppler's tools with E1's fontconfig — first on PATH for the runner
# and compat-attribute.py only; kasane reads no fontconfig either way.
# Arguments, DPI and timeouts are the nightly's. The scripts run are
# SRC's own (scripts/compat-attribute.py), as in the nightly.
set -eu
SRC=$(cd "$1" && pwd); CORPUS=$(cd "$2" && pwd); OUT=$3; ENV=$4
mkdir -p "$OUT"; OUT=$(cd "$OUT" && pwd)
# The runner reads CORPUS/../manifest.yaml (each file's expected outcome):
# without it no file is `expect: pass` and the second witness checks none.
[ -f "$CORPUS/../manifest.yaml" ] || { echo "run-2x2: no manifest.yaml next to $CORPUS" >&2; exit 2; }
T=${CARGO_TARGET_DIR:-$OUT/target}
( cd "$SRC/compat/runner" && CARGO_TARGET_DIR=$T cargo build -q --locked --release --features cjk-fallback )
( cd "$SRC" && CARGO_TARGET_DIR=$T/cjk cargo build -q --locked --release --features cjk-fallback --bin kasane-pdf )
case "$ENV" in
  e0) P=$PATH ;;
  e1) P=$(cd "${5:?E1DIR}" && pwd)/bin:$PATH; [ -x "${P%%:*}/pdftoppm" ] || { echo "no E1 at $5" >&2; exit 2; } ;;
  *) echo "e0 or e1" >&2; exit 2 ;;
esac
[ -z "$(git -C "$SRC" status --porcelain)" ] || { echo "run-2x2: $SRC has changes" >&2; exit 2; }
git -C "$SRC" rev-parse HEAD > "$OUT/implementation.sha"
git -C "$(dirname "$0")" rev-parse HEAD > "$OUT/harness.sha"
echo "$ENV" > "$OUT/environment"
# E1's configuration as used (its hashes), nothing for E0.
if [ "$ENV" = e1 ]; then sha256sum "$5/fonts.conf" "$5"/fonts/* > "$OUT/reference-fonts.sha256"; else : > "$OUT/reference-fonts.sha256"; fi
mkdir -p "$OUT/run"
PATH=$P "$T/release/kasane-compat" run --corpus "$CORPUS" --out "$OUT/run" --with-poppler --dpi 96 --timeout-secs 20 \
  --mode full --rewrite-options carry > "$OUT/run.log" 2>&1
( cd "$SRC" && PATH=$P KASANE_CORPUS_DIR=$CORPUS python3 scripts/compat-attribute.py "$OUT/run/latest.json" "$T/cjk/release/kasane-pdf" \
    --out "$OUT/run/attribution.json" --summary "$OUT/run/attribution-summary.json" --features cjk-fallback --jobs 4 \
    > "$OUT/attribute.log" 2>&1 )
echo "$ENV $(cat "$OUT/implementation.sha" | cut -c1-12): done"
