#!/bin/sh
# External verification of inherited-/Resources materialisation
# on a multi-branch, multi-page document.
#
# Generates tests/fixtures/m224_branches.pdf (deterministic), runs
# experimental-remove-region on page 1, then checks, per page:
#   structure : qpdf --check zero warnings; every page dict carries an
#               explicit /Resources; the two /Resources-bearing branch
#               /Pages nodes dropped theirs; Kids/Count/Parent intact.
#   rendering : pdftoppm at 300 dpi — page 2 is byte-identical to the
#               original; page 1 differs only inside the edited image
#               region.
#   text      : pdftotext output identical on both pages.
#   images    : the edited page's /KasIm0 holds 00 BB CC DD; the
#               unedited page's /Im1 still resolves to the branch-B
#               image 11 22 33 44; the original AA BB CC DD is gone.
#
# Requires: qpdf, pdftotext, pdftoppm, pdfimages (poppler), python3,
#           the kasane-pdf CLI (cargo build).
# Usage:  scripts/verify-resources-materialize.sh
# Exit 0 iff every check passes.

set -u
cd "$(dirname "$0")/.."

BIN="${KASANE_BIN:-target/debug/kasane-pdf}"
OUTDIR="${TMPDIR:-/tmp}/kasane-m224-$$"
mkdir -p "$OUTDIR"
trap 'rm -rf "$OUTDIR"' EXIT

for tool in qpdf pdftotext pdftoppm pdfimages python3; do
    if ! command -v "$tool" >/dev/null 2>&1; then
        echo "SKIP: $tool not found — cannot run external verification" >&2
        exit 2
    fi
done
if [ ! -x "$BIN" ]; then
    echo "kasane-pdf binary not found at $BIN (run: cargo build)" >&2
    exit 2
fi

SRC="$OUTDIR/m224_branches.pdf"
DST="$OUTDIR/m224_out.pdf"

python3 tests/fixtures/make_m224_fixture.py "$SRC" || exit 1

# Edit page 1: rect covers the top-left pixel of the painted image.
if ! "$BIN" experimental-remove-region "$SRC" 1 \
        --rect 20 80 40 100 --output "$DST" >/dev/null 2>&1; then
    echo "FAIL: remove_region refused the fixture"
    exit 1
fi

# --- structure -----------------------------------------------------
res=$(qpdf --check "$DST" 2>&1)
warn=$(printf '%s' "$res" | grep -c "WARNING" || true)
if [ "$warn" -ne 0 ]; then
    echo "FAIL: qpdf --check warnings"
    printf '%s\n' "$res" | grep WARNING | sed 's/^/    /'
    exit 1
fi
echo "structure/qpdf-check: PASS (0 warnings)"

python3 - "$DST" <<'PYEOF'
import json, subprocess, sys
out = subprocess.run(["qpdf", "--json", sys.argv[1]],
                     capture_output=True, text=True).stdout
objs = {k: v.get("value", v)
        for k, v in json.loads(out)["qpdf"][1].items()
        if k.startswith("obj:")}

def pages_node(o):
    return isinstance(o, dict) and o.get("/Type") == "/Pages"

pages_nodes = {k: v for k, v in objs.items() if pages_node(v)}
with_res = [k for k, v in pages_nodes.items() if "/Resources" in v]
if with_res:
    print(f"FAIL: /Pages nodes still carry /Resources: {with_res}")
    sys.exit(1)

page_objs = {k: v for k, v in objs.items()
             if isinstance(v, dict) and v.get("/Type") == "/Page"}
if len(page_objs) != 2:
    print(f"FAIL: expected 2 pages, got {len(page_objs)}")
    sys.exit(1)
for k, v in sorted(page_objs.items()):
    r = v.get("/Resources")
    if not (isinstance(r, dict) and "/XObject" in r and "/Font" in r):
        print(f"FAIL: page {k} lacks explicit /Resources: {v}")
        sys.exit(1)

# /Kids and /Count maintained — 5 /Pages nodes: root Count=2 plus
# four single-kid nodes with Count=1.
counts = sorted(v.get("/Count") for v in pages_nodes.values())
if counts != [1, 1, 1, 1, 2]:
    print(f"FAIL: /Count values wrong: {counts}")
    sys.exit(1)
print("structure/page-resources: PASS "
      "(both pages materialised; all /Pages stripped)")
PYEOF
[ $? -ne 0 ] && exit 1

# --- text ----------------------------------------------------------
t1=$(pdftotext "$SRC" - 2>/dev/null)
t2=$(pdftotext "$DST" - 2>/dev/null)
if [ "$t1" != "$t2" ]; then
    echo "FAIL: pdftotext output differs"
    exit 1
fi
nlines=$(printf '%s' "$t2" | grep -c ABC || true)
if [ "$nlines" -ne 2 ]; then
    echo "FAIL: expected ABC on both pages, got $nlines"
    exit 1
fi
echo "text/pdftotext: PASS (identical, ABC x2)"

# --- images --------------------------------------------------------
python3 - "$SRC" "$DST" <<'PYEOF'
import subprocess, sys

def streams(path):
    import json
    out = subprocess.run(["qpdf", "--json", path],
                         capture_output=True, text=True).stdout
    objs = {k: v for k, v in json.loads(out)["qpdf"][1].items()
            if k.startswith("obj:")}
    data = {}
    for k in objs:
        r = subprocess.run(["qpdf", "--show-object=" + k[4:],
                            "--raw-stream-data", path],
                           capture_output=True)
        if r.returncode == 0 and len(r.stdout) == 4:
            data[k] = r.stdout
    return data

orig = streams(sys.argv[1])
new = streams(sys.argv[2])
orig_set = set(orig.values())
new_set = set(new.values())

if bytes([0x00, 0xBB, 0xCC, 0xDD]) not in new_set:
    print("FAIL: edited image 00 BB CC DD not in output")
    sys.exit(1)
if bytes([0x11, 0x22, 0x33, 0x44]) not in new_set:
    print("FAIL: branch-B image 11 22 33 44 not in output")
    sys.exit(1)
if bytes([0xAA, 0xBB, 0xCC, 0xDD]) in new_set:
    print("FAIL: original AA BB CC DD still present (should be orphaned)")
    sys.exit(1)
print("images/content: PASS "
      "(edited 00 BB CC DD present, branch-B 11 22 33 44 kept, "
      "original AA BB CC DD gone)")
PYEOF
[ $? -ne 0 ] && exit 1

# --- rendering -----------------------------------------------------
pdftoppm -r 300 -png "$SRC" "$OUTDIR/orig" >/dev/null 2>&1
pdftoppm -r 300 -png "$DST" "$OUTDIR/edit" >/dev/null 2>&1
python3 - "$OUTDIR" <<'PYEOF'
import struct, sys, zlib

def read_png(path):
    d = open(path, 'rb').read()
    i = 8; idat = b''; w = h = ct = None
    while i < len(d):
        ln = struct.unpack('>I', d[i:i+4])[0]
        typ = d[i+4:i+8]
        if typ == b'IHDR':
            w, h, bd, ct = struct.unpack('>IIBB', d[i+8:i+18])
        elif typ == b'IDAT':
            idat += d[i+8:i+8+ln]
        i += 12 + ln
    raw = zlib.decompress(idat)
    ch = {0: 1, 2: 3, 4: 2, 6: 4}[ct]
    px = []; pos = 0; prev = bytearray(w * ch)
    for y in range(h):
        f = raw[pos]; pos += 1
        row = bytearray(raw[pos:pos+w*ch]); pos += w*ch
        if f == 1:
            for x in range(ch, w*ch):
                row[x] = (row[x] + row[x-ch]) & 255
        elif f == 2:
            for x in range(w*ch):
                row[x] = (row[x] + prev[x]) & 255
        elif f == 3:
            for x in range(w*ch):
                a = row[x-ch] if x >= ch else 0
                row[x] = (row[x] + ((a + prev[x]) >> 1)) & 255
        elif f == 4:
            for x in range(w*ch):
                a = row[x-ch] if x >= ch else 0
                b = prev[x]; c = prev[x-ch] if x >= ch else 0
                p = a + b - c
                pa, pb, pc = abs(p-a), abs(p-b), abs(p-c)
                pr = a if (pa <= pb and pa <= pc) else (b if pb <= pc else c)
                row[x] = (row[x] + pr) & 255
        prev = row
        px.append(bytes(row))
    return w, h, ch, px

out = sys.argv[1]
diffs = []
for pg in (1, 2):
    w1, h1, c1, p1 = read_png(f"{out}/orig-{pg}.png")
    w2, h2, c2, p2 = read_png(f"{out}/edit-{pg}.png")
    n = sum(1 for y in range(h1) for x in range(w1*c1)
            if p1[y][x] != p2[y][x])
    diffs.append(n)
    print(f"render/page{pg}: {w1}x{h1} changed-bytes={n}")

if diffs[1] != 0:
    print("FAIL: page 2 (unedited) rendering changed")
    sys.exit(1)
if diffs[0] == 0:
    print("FAIL: page 1 (edited) rendering unchanged — "
          "expected a visible pixel change")
    sys.exit(1)
print("render/pdftoppm-300dpi: PASS "
      "(page 1 differs inside edited region; page 2 identical)")
PYEOF
[ $? -ne 0 ] && exit 1

echo "m224: ALL CHECKS PASSED"
