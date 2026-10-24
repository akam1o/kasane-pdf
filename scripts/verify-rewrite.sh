#!/bin/sh
# Warning-free structural validation of rewrite output.
# Requires: qpdf, the kasane-pdf CLI built (cargo build).
# Usage:  scripts/verify-rewrite.sh
# Prints one line per fixture:  "name: PASS" or "name: <detail>".
# Exit 0 iff every fixture passes qpdf --check with zero warnings.

set -u
cd "$(dirname "$0")/.."

BIN="${KASANE_BIN:-target/debug/kasane-pdf}"
OUTDIR="${TMPDIR:-/tmp}/kasane-verify-$$"
mkdir -p "$OUTDIR"
trap 'rm -rf "$OUTDIR"' EXIT

if ! command -v qpdf >/dev/null 2>&1; then
    echo "qpdf not found — cannot run independent structural check" >&2
    exit 2
fi
if [ ! -x "$BIN" ]; then
    echo "kasane-pdf binary not found at $BIN (run: cargo build)" >&2
    exit 2
fi

# Well-formed fixtures that must validate with zero warnings.
# hybrid.pdf is intentionally excluded — it is an adversarial
# fixture (the xref stream marks object 3 free) and does not rewrite.
CLEAN="hello text_basic pages incremental xref_stream objstm \
hybrid_compressed hybrid_gap xobject_form_basic xobject_form_nested \
xobject_image_gray_raw xobject_image_rgb_flate \
xobject_image_decode_invert xobject_image_smask \
render_text render_rects contents_array text_tj \
text_same_name_two_resources text_tc_tw_tz_ts text_rotated \
text_differences_encoding text_composite_glyph"

fail=0
for f in $CLEAN; do
    src="tests/fixtures/$f.pdf"
    dst="$OUTDIR/$f.pdf"
    if ! "$BIN" rewrite "$src" --output "$dst" >/dev/null 2>&1; then
        echo "$f: REWRITE-REFUSED"
        fail=1
        continue
    fi
    res=$(qpdf --check "$dst" 2>&1)
    code=$?
    warn=$(printf '%s' "$res" | grep -c "WARNING")
    if [ "$code" -eq 0 ] && [ "$warn" -eq 0 ]; then
        echo "$f: PASS"
    else
        echo "$f: FAIL (exit=$code warnings=$warn)"
        printf '%s\n' "$res" | grep "WARNING" | sed 's/^/    /'
        fail=1
    fi
done
exit "$fail"
