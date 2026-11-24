#!/usr/bin/env python3
"""Independent check that a redacted page leaves nothing inside a rect but
zeroed image samples. Shares no code with kasane-pdf: qpdf, Poppler and
MuPDF only. Used to settle `eval-redact-corpus.py`'s known false signal
(unchanged ink inside the rect that is all image samples): a zeroed
sample may be drawn black, which a render alone cannot tell from a
sample left as it was.

    scripts/check-zeroed-images.py OUT.pdf PAGE llx lly urx ury [--frame ROT x0 y0 x1 y1] [--dpi 100]
    (rect in the displayed frame when --frame is given, else default user space)

Method, on the output only:
 1. qpdf JSON (v2) of every object, streams decoded (--decode-level=all,
    lossy included). An image whose data qpdf cannot decode (JBIG2, JPX,
    CCITT left encoded) -> `unhandled`.
 2. Every image XObject (inline images: `unhandled`) is rewritten as 8-bit
    DeviceGray of the same size: a sample whose stored components are all 0
    becomes white, any other sample black. /SMask, /Mask, /Decode,
    /SMaskInData, /Interpolate and /Intent go, so nothing masks or remaps a
    residue. /ImageMask stencils are kept as they are (they paint: any ink
    they leave inside counts).
 3. Every ExtGState is made opaque and Normal (/ca /CA 1, /BM /Normal,
    /SMask /None) and the catalog's /OCProperties is removed (every
    optional content shown), so neither transparency nor optional content
    hides a residue.
 4. The rewritten file is rendered by Poppler and by MuPDF; a non-white
    pixel strictly inside the rect (2 px margin) is ink left there: a
    non-zero image sample or anything else.

Verdict: `zero` (both renderers: no ink inside), `ink:<renderer>:<n>`,
`unhandled:<why>`, `error:<what>` (a tool failed or timed out). Only
`zero` says the rect holds nothing but zero samples.
"""
import base64, json, os, re, subprocess, sys, tempfile

TIMEOUT = 300


def run(a):
    try:
        return subprocess.run(a, capture_output=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        return None


COMPONENTS = {"/DeviceGray": 1, "/G": 1, "/CalGray": 1, "/DeviceRGB": 3, "/RGB": 3, "/CalRGB": 3,
              "/Lab": 3, "/DeviceCMYK": 4, "/CMYK": 4, "/Indexed": 1, "/I": 1, "/Separation": 1}


def components(cs, objs, depth=0):
    """Components of a colour space value from qpdf JSON; None: unknown."""
    if depth > 8:
        return None
    if isinstance(cs, str) and cs.endswith(" R"):
        o = objs.get("obj:" + cs)
        return None if o is None else components(o.get("value"), objs, depth + 1)
    if isinstance(cs, str):
        return COMPONENTS.get(cs)
    if isinstance(cs, list) and cs:
        fam = cs[0]
        if fam in ("/ICCBased",) and len(cs) > 1:
            s = objs.get("obj:" + cs[1]) if isinstance(cs[1], str) else None
            n = (s or {}).get("stream", {}).get("dict", {}).get("/N")
            return n if isinstance(n, int) else None
        if fam == "/DeviceN" and len(cs) > 1:
            names = cs[1]
            if isinstance(names, str) and names.endswith(" R"):
                names = (objs.get("obj:" + names) or {}).get("value")
            return len(names) if isinstance(names, list) else None
        return COMPONENTS.get(fam)
    return None


def unpack(data, w, h, n, bpc):
    """Per-sample flags: all components zero. None on a size mismatch."""
    row = (w * n * bpc + 7) // 8
    if len(data) < row * h:
        return None
    out = bytearray(w * h)
    for y in range(h):
        r = data[y * row:(y + 1) * row]
        for x in range(w):
            zero = True
            for c in range(n):
                i = (x * n + c) * bpc
                if bpc == 8:
                    v = r[i // 8]
                elif bpc == 16:
                    v = r[i // 8] << 8 | r[i // 8 + 1]
                else:
                    v = (r[i // 8] >> (8 - bpc - i % 8)) & ((1 << bpc) - 1)
                if v:
                    zero = False
                    break
            out[y * w + x] = 255 if zero else 0
    return bytes(out)


def neutralise(v, depth=0, top=False):
    """Every graphics state dictionary inside `v` (direct ones in
    resources included) made opaque and Normal, its soft mask removed."""
    if depth > 64:
        return
    if isinstance(v, dict):
        is_image = v.get("/Subtype") == "/Image"
        if not is_image and (v.get("/Type") == "/ExtGState" or any(x in v for x in ("/ca", "/CA", "/BM"))):
            for x in ("/ca", "/CA"):
                if x in v:
                    v[x] = 1
            if "/BM" in v:
                v["/BM"] = "/Normal"
            if "/SMask" in v:
                v["/SMask"] = "/None"
        for x in v.values():
            neutralise(x, depth + 1)
    elif isinstance(v, list):
        for x in v:
            neutralise(x, depth + 1)


def rewrite(src, dst, tmp):
    j = os.path.join(tmp, "o.json")
    r = run(["qpdf", "--json=2", "--json-stream-data=file", "--json-stream-prefix=" + os.path.join(tmp, "s"),
             "--decode-level=all", src, j])
    if r is None:
        return "error:qpdf-timeout"
    if r.returncode not in (0, 3):
        return "error:qpdf-json"
    d = json.load(open(j))
    objs = d["qpdf"][1]
    for k, o in list(objs.items()):
        if "stream" in o:
            sd = o["stream"]["dict"]
            if sd.get("/Subtype") == "/Image":
                if sd.get("/ImageMask") is True:
                    continue
                if "/Filter" in sd:
                    return "unhandled:filter " + str(sd.get("/Filter"))
                w, h, bpc = sd.get("/Width"), sd.get("/Height"), sd.get("/BitsPerComponent")
                n = components(sd.get("/ColorSpace"), objs)
                if sd.get("/SMaskInData") not in (None, 0):
                    return "unhandled:SMaskInData"
                if not all(isinstance(v, int) for v in (w, h, bpc)) or bpc not in (1, 2, 4, 8, 16) or not n:
                    return "unhandled:image geometry or colour space"
                data = open(o["stream"]["datafile"], "rb").read()
                flags = unpack(data, w, h, n, bpc)
                if flags is None:
                    return "unhandled:short image data"
                for key in ("/SMask", "/Mask", "/Decode", "/SMaskInData", "/Interpolate", "/Intent",
                            "/DecodeParms", "/Alternates"):
                    sd.pop(key, None)
                sd["/ColorSpace"] = "/DeviceGray"
                sd["/BitsPerComponent"] = 8
                open(o["stream"]["datafile"], "wb").write(flags)
            continue
        v = o.get("value")
        if isinstance(v, dict) and v.get("/Type") == "/Catalog":
            v.pop("/OCProperties", None)
        if v is not None:
            neutralise(v)
        elif "stream" in o:
            neutralise(o["stream"]["dict"], top=True)
    # Inline images are not rewritten: refuse to judge a page with one.
    for k, o in objs.items():
        if "stream" in o and o["stream"]["dict"].get("/Subtype") is None and "datafile" in o["stream"]:
            data = open(o["stream"]["datafile"], "rb").read()
            if re.search(rb"(^|\s)BI\s", data):
                return "unhandled:inline image"
    json.dump(d, open(j, "w"))
    r = run(["qpdf", "--json-input", j, dst])
    if r is None:
        return "error:qpdf-timeout"
    if r.returncode not in (0, 3):
        return "error:qpdf-write"
    return None


def pgm(p):
    data = open(p, "rb").read()
    fields, i = [], 0
    while len(fields) < 4:
        while data[i:i + 1].isspace():
            i += 1
        if data[i:i + 1] == b"#":
            i = data.index(b"\n", i) + 1
            continue
        j = i
        while not data[j:j + 1].isspace():
            j += 1
        fields.append(data[i:j]); i = j
    w, h = int(fields[1]), int(fields[2])
    raster = data[i + 1:]
    if fields[0] != b"P5" or len(raster) != w * h:
        raise ValueError("pgm")
    return w, h, raster


def to_display(rect, frame):
    """A user-space rect in the displayed frame (lower-left origin)."""
    if frame is None:
        return rect
    rot, (x0, y0, x1, y1) = frame
    def pt(u, v):
        return {0: (u - x0, v - y0), 90: (v - y0, x1 - u), 180: (x1 - u, y1 - v), 270: (y1 - v, u - x0)}[rot]
    a, b = pt(rect[0], rect[1]), pt(rect[2], rect[3])
    return (min(a[0], b[0]), min(a[1], b[1]), max(a[0], b[0]), max(a[1], b[1]))


def check(out_pdf, page, rect, frame=None, dpi=100):
    """`rect`: in the displayed frame (with `frame`) or default user space."""
    with tempfile.TemporaryDirectory() as tmp:
        fixed = os.path.join(tmp, "fixed.pdf")
        why = rewrite(out_pdf, fixed, tmp)
        if why:
            return why
        s = dpi / 72.0
        for who in ("poppler", "mupdf"):
            base = os.path.join(tmp, who)
            if who == "poppler":
                r = run(["pdftoppm", "-cropbox", "-r", str(dpi), "-gray", "-f", str(page), "-l", str(page),
                         "-singlefile", fixed, base])
            else:
                r = run(["mutool", "draw", "-q", "-r", str(dpi), "-c", "gray", "-o", base + ".pgm", fixed, str(page)])
            if r is None:
                return f"error:{who}-timeout"
            if r.returncode != 0 or not os.path.exists(base + ".pgm"):
                return f"error:{who}"
            try:
                w, h, px = pgm(base + ".pgm")
            except (ValueError, IndexError):
                return f"error:{who}-pgm"
            x0, y0, x1, y1 = rect
            n = 0
            for y in range(max(0, int(h - y1 * s) + 2), min(h, int(h - y0 * s) - 2)):
                for x in range(max(0, int(x0 * s) + 2), min(w, int(x1 * s) - 2)):
                    if px[y * w + x] < 255:
                        n += 1
            if n:
                return f"ink:{who}:{n}"
    return "zero"


if __name__ == "__main__":
    a = sys.argv[1:]
    frame = None
    if "--frame" in a:
        i = a.index("--frame")
        frame = (int(a[i + 1]), [float(v) for v in a[i + 2:i + 6]])
        del a[i:i + 6]
    dpi = 100
    if "--dpi" in a:
        i = a.index("--dpi"); dpi = int(a[i + 1]); del a[i:i + 2]
    print(check(a[0], int(a[1]), [float(v) for v in a[2:6]], frame, dpi))
