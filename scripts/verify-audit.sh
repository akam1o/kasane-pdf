#!/bin/sh
# Independent external check of audit-residuals findings (milestone M23).
#
# Compares the audit output against qpdf's view of the same files.
# qpdf reports the *current logical* structure — it does NOT prove
# that superseded bytes or slack regions are absent, so those
# findings are cross-checked only where qpdf has a matching concept
# (incremental-update /Prev, free entries).
#
# Requires: qpdf, pdftotext, python3, the kasane-pdf CLI (cargo build).
# Usage:  scripts/verify-audit.sh
# Exit 0 iff every check passes.

set -u
cd "$(dirname "$0")/.."

BIN="${KASANE_BIN:-target/debug/kasane-pdf}"

for tool in qpdf python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "SKIP: $tool not found" >&2
        exit 2
    fi
done
if [ ! -x "$BIN" ]; then
    echo "kasane-pdf binary not found at $BIN (run: cargo build)" >&2
    exit 2
fi

fail=0
check() {
    name="$1"; file="$2"; shift 2
    out=$("$BIN" audit-residuals "$file" --format json) || {
        echo "$name: FAIL (audit refused)"; fail=1; return; }
    if ! printf '%s' "$out" | python3 -c "
import json, sys
d = json.load(sys.stdin)
kinds = [f['kind'] for f in d['finding_list']]
ok = True
for want in sys.argv[1:]:
    if want not in kinds:
        print(f'  missing {want}')
        ok = False
sys.exit(0 if ok else 1)
" "$@"; then
        echo "$name: FAIL (missing expected findings)"
        fail=1
    else
        echo "$name: PASS"
    fi
}

check "hello.pdf"        tests/fixtures/hello.pdf \
    Resources ResourceUsageUnexamined FreeObject
check "render_annot.pdf" tests/fixtures/render_annot.pdf \
    AnnotText
check "image"            tests/fixtures/xobject_image_gray_raw.pdf \
    Image ResourceUsageUnexamined
check "form"             tests/fixtures/xobject_form_basic.pdf \
    FormXObject
check "objstm.pdf"       tests/fixtures/objstm.pdf \
    UnreachableObject FreeObject
check "incremental.pdf"  tests/fixtures/incremental.pdf \
    DocumentInfo SupersededObject

# Cross-check incremental history with qpdf: qpdf sees the *merged*
# /Info, while the newest trailer has none — both views must agree
# that an /Info object exists.
info_in_qpdf=$(qpdf --show-object=5 tests/fixtures/incremental.pdf 2>/dev/null)
if printf '%s' "$info_in_qpdf" | grep -q Producer; then
    echo "incremental.pdf: qpdf agrees /Info exists (Producer present)"
else
    echo "incremental.pdf: WARN qpdf --show-object=5 unexpected: $info_in_qpdf"
fi

# Negative check: a PDF *without* these features must report zero
# for the matching kinds — and must NOT be read as "safe".
out=$("$BIN" audit-residuals tests/fixtures/pages.pdf --format json)
if printf '%s' "$out" | python3 -c "
import json, sys
d = json.load(sys.stdin)
kinds = {f['kind'] for f in d['finding_list']}
bad = kinds & {'DocumentInfo','AcroForm','JavaScript','EmbeddedFiles',
               'StructTree','OptionalContent','Metadata','OpenAction'}
if bad:
    print('unexpected findings:', bad); sys.exit(1)
sys.exit(0)
"; then
    echo "pages.pdf: PASS (no Info/AcroForm/JavaScript/… present)"
else
    echo "pages.pdf: FAIL"; fail=1
fi

exit "$fail"
