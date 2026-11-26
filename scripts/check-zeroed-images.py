#!/usr/bin/env python3
"""Structural check that a redacted page keeps no image sample under a rect.

    scripts/check-zeroed-images.py INPUT.pdf OUTPUT.pdf PAGE llx lly urx ury [--elsewhere]

Shares no code with kasane-pdf (qpdf only). It settles one question
`eval-redact-corpus.py` cannot answer from a render: whether the ink left
inside a rect is zeroed image samples (a zeroed sample may be drawn black,
so a render cannot tell it from a sample left as it was). It does not
look at the render at all, and it says nothing about text or paths: those
remain the text and pixel checks' (a `zero` here never replaces them).

Method (the rect in default user space of PAGE, 1-based):

 1. qpdf JSON v2 of each file, every losslessly filtered stream decoded
    (`--decode-level=specialized`: a DCT, JPX, JBIG2 or CCITT image stays
    encoded and is `unhandled` — a lossy re-encoding of zero is not 0).
    qpdf repairing either file (exit 3), or failing, is `unhandled`.
 2. The output page's content and, recursively, the forms it draws are
    read with a tokenizer of their own: `q`/`Q`, `cm`, `Do`, a form's
    `/Matrix`; resources inherited as §7.7.3.4 and §8.10.1 say. Every
    image an invocation draws gets its CTM. Clips are ignored (a clip
    hides ink, not data: a sample under the rect counts wherever a clip
    is). An inline image, a pattern colour, a Type 3 font, a soft mask
    of a graphics state (its form may draw images), an annotation
    of the page with an appearance, a form or image reached through
    something else than `Do`: `unhandled` — samples there are not
    followed.
 3. For each image drawn whose unit square meets the rect: its decoded
    samples (a filter qpdf could not remove: `unhandled`), the CTM
    axis-aligned or a quarter turn (else `unhandled`). Every sample whose
    cell overlaps the rect's interior must hold the value that does not
    depend on the original: 0 in every component for a colour image
    (whatever /Decode or palette makes of it), and for an image mask
    (/ImageMask true) the value that paints nothing (1 under /Decode
    absent or [0 1], 0 under [1 0]; another /Decode: `unhandled`). Its
    /SMask, if any, must be 255 (opaque) at those samples, at the mask's
    own grid; a stencil /Mask or colour-key /Mask: `unhandled`.
 4. No stream of the output may hold the stored or decoded bytes of an
    image the input page drew (the original), unless `--elsewhere` says
    the redaction reported an original still drawn elsewhere.

Verdict on stdout: `zero`; `residue:<what>` (a sample under the rect not
at its value, or an original carried); `unhandled:<why>`. Exit 0 only for
`zero`. Only `zero` settles the false signal.
"""
import hashlib
import json
import os
import re
import subprocess
import sys
import tempfile

TIMEOUT = 300
# A cell meets the rect only by more than this (pt): a cell boundary on
# the rect's edge, computed in floating point, lands a few ulps either
# side of it.
EPS = 1e-6


class Unhandled(Exception):
    pass


class Residue(Exception):
    pass


def qpdf_json(pdf, tmp, tag):
    j = os.path.join(tmp, tag + ".json")
    try:
        r = subprocess.run(["qpdf", "--json=2", "--json-stream-data=file",
                            "--json-stream-prefix=" + os.path.join(tmp, tag),
                            "--decode-level=specialized", pdf, j], capture_output=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        raise Unhandled(f"qpdf timed out on the {tag}")
    if r.returncode != 0:
        raise Unhandled(f"qpdf exit {r.returncode} on the {tag} (repaired or unreadable)")
    with open(j) as fh:
        return json.load(fh)


class Doc:
    def __init__(self, j):
        self.j = j
        self.objs = j["qpdf"][1]

    def raw(self, ref):
        return self.objs.get("obj:" + ref)

    def val(self, v, depth=0):
        """A value with references followed (at most 32 links)."""
        while isinstance(v, str) and re.fullmatch(r"\d+ \d+ R", v):
            if depth > 32:
                raise Unhandled("reference chain")
            o = self.raw(v)
            if o is None:
                return None
            v = o["stream"]["dict"] if "stream" in o else o.get("value")
            depth += 1
        return v

    def stream(self, ref):
        o = self.raw(ref) if isinstance(ref, str) else None
        if not o or "stream" not in o:
            return None, None
        return o["stream"]["dict"], o["stream"].get("datafile")

    def data(self, ref):
        d, f = self.stream(ref)
        if f is None:
            raise Unhandled(f"stream {ref} has no data")
        with open(f, "rb") as fh:
            return fh.read()


def num(v):
    if isinstance(v, bool) or not isinstance(v, (int, float)):
        raise Unhandled("a number expected")
    return float(v)


# ---- content tokenizer -------------------------------------------------

DELIM = b"()<>[]{}/%"
WS = b" \t\r\n\f\x00"


def tokens(data):
    """(kind, value): ('op', b'cm'), ('num', 1.0), ('name', b'Im0'),
    ('other', None). An inline image raises Unhandled."""
    i, n = 0, len(data)
    while i < n:
        c = data[i:i + 1]
        if c in (b" ", b"\t", b"\r", b"\n", b"\f", b"\x00"):
            i += 1
        elif c == b"%":
            while i < n and data[i:i + 1] not in (b"\r", b"\n"):
                i += 1
        elif c == b"(":
            depth, i = 1, i + 1
            while i < n and depth:
                ch = data[i:i + 1]
                if ch == b"\\":
                    i += 2
                    continue
                depth += {b"(": 1, b")": -1}.get(ch, 0)
                i += 1
            if depth:
                raise Unhandled("unterminated string")
            yield "other", None
        elif c == b"<" and data[i + 1:i + 2] == b"<":
            i += 2
            yield "dict<", None
        elif c == b">" and data[i + 1:i + 2] == b">":
            i += 2
            yield "dict>", None
        elif c == b"<":
            j = data.find(b">", i)
            if j < 0:
                raise Unhandled("unterminated hex string")
            i = j + 1
            yield "other", None
        elif c in (b"[", b"]"):
            i += 1
            yield c.decode(), None
        elif c in (b"{", b"}", b")", b">"):
            raise Unhandled("unexpected delimiter in content")
        elif c == b"/":
            j = i + 1
            while j < n and data[j:j + 1] not in WS and data[j] not in DELIM:
                j += 1
            name = re.sub(rb"#([0-9A-Fa-f]{2})", lambda m: bytes([int(m.group(1), 16)]), data[i + 1:j])
            i = j
            yield "name", name
        else:
            j = i
            while j < n and data[j:j + 1] not in WS and data[j] not in DELIM:
                j += 1
            word = data[i:j]
            i = j
            if re.fullmatch(rb"[+-]?(\d+\.?\d*|\.\d+)", word):
                yield "num", float(word)
            elif word == b"BI":
                raise Unhandled("inline image")
            else:
                yield "op", word


# ---- the walk ----------------------------------------------------------

def mul(m, n):
    a, b, c, d, e, f = m
    A, B, C, D, E, F = n
    return (a * A + b * C, a * B + b * D, c * A + d * C, c * B + d * D, e * A + f * C + E, e * B + f * D + F)


def resources_of_page(doc, page_ref):
    node, seen = page_ref, set()
    while node and node not in seen:
        seen.add(node)
        o = doc.val(node)
        if not isinstance(o, dict):
            raise Unhandled("page node")
        if "/Resources" in o:
            return doc.val(o["/Resources"]) or {}
        node = o.get("/Parent")
    return {}


def walk(doc, page_ref):
    """[(image ref, ctm)] the page's content draws, through forms."""
    page = doc.val(page_ref)
    if page.get("/Annots"):
        for a in doc.val(page["/Annots"]) or []:
            a = doc.val(a) or {}
            if a.get("/AP") is not None:
                raise Unhandled("an annotation with an appearance on the page")
    contents = page.get("/Contents")
    refs = contents if isinstance(contents, list) else ([contents] if contents else [])
    data = b"\n".join(doc.data(r) for r in refs)
    out = []
    run(doc, data, resources_of_page(doc, page_ref), (1, 0, 0, 1, 0, 0), out, [], 0)
    return out


def check_resources(doc, res):
    for key in ("/Pattern",):
        if doc.val(res.get(key)):
            raise Unhandled("pattern resources (samples there are not followed)")
    for gs in (doc.val(res.get("/ExtGState")) or {}).values():
        sm = (doc.val(gs) or {}).get("/SMask")
        if sm is not None and doc.val(sm) != "/None":
            raise Unhandled("a graphics-state soft mask")
    for f in (doc.val(res.get("/Font")) or {}).values():
        if (doc.val(f) or {}).get("/Subtype") == "/Type3":
            raise Unhandled("a Type 3 font")


def run(doc, data, res, ctm, out, chain, depth):
    if depth > 16:
        raise Unhandled("forms nested more than 16 deep")
    check_resources(doc, res)
    stack, operands = [], []
    for kind, v in tokens(data):
        if kind == "op":
            if v == b"q":
                stack.append(ctm)
            elif v == b"Q":
                if not stack:
                    raise Unhandled("Q without q")
                ctm = stack.pop()
            elif v == b"cm":
                if len(operands) < 6 or any(k != "num" for k, _ in operands[-6:]):
                    raise Unhandled("cm without six numbers")
                ctm = mul(tuple(x for _, x in operands[-6:]), ctm)
            elif v == b"Do":
                if not operands or operands[-1][0] != "name":
                    raise Unhandled("Do without a name")
                name = "/" + operands[-1][1].decode("latin-1")
                xo = (doc.val(res.get("/XObject")) or {}).get(name)
                if not isinstance(xo, str):
                    raise Unhandled(f"XObject {name} not an indirect stream")
                d, _ = doc.stream(xo)
                if d is None:
                    raise Unhandled(f"XObject {name} is not a stream")
                sub = doc.val(d.get("/Subtype"))
                if sub == "/Image":
                    out.append((xo, ctm, res))
                elif sub == "/Form":
                    if xo in chain:
                        raise Unhandled("a form drawing itself")
                    m = doc.val(d.get("/Matrix")) or [1, 0, 0, 1, 0, 0]
                    m = tuple(num(doc.val(x)) for x in m)
                    if len(m) != 6:
                        raise Unhandled("form /Matrix")
                    fres = doc.val(d.get("/Resources"))
                    run(doc, doc.data(xo), fres if fres is not None else res, mul(m, ctm), out,
                        chain + [xo], depth + 1)
                else:
                    raise Unhandled(f"XObject subtype {sub}")
            elif v in (b"scn", b"SCN") and operands and operands[-1][0] == "name":
                raise Unhandled("a pattern colour")
            operands = []
        else:
            operands.append((kind, v))


# ---- samples -----------------------------------------------------------

COMPONENTS = {"/DeviceGray": 1, "/G": 1, "/CalGray": 1, "/DeviceRGB": 3, "/RGB": 3, "/CalRGB": 3,
              "/Lab": 3, "/DeviceCMYK": 4, "/CMYK": 4, "/Indexed": 1, "/I": 1, "/Separation": 1}


def components(doc, cs, res, depth=0):
    if depth > 8:
        raise Unhandled("colour space chain")
    cs = doc.val(cs)
    if isinstance(cs, str) and cs in COMPONENTS:
        return COMPONENTS[cs]
    if isinstance(cs, str):
        named = (doc.val(res.get("/ColorSpace")) or {}).get(cs)
        if named is None:
            raise Unhandled(f"colour space {cs}")
        return components(doc, named, res, depth + 1)
    if isinstance(cs, list) and cs:
        fam = doc.val(cs[0])
        if fam == "/ICCBased":
            d, _ = doc.stream(cs[1]) if len(cs) > 1 else (None, None)
            n = (d or {}).get("/N")
            if n in (1, 3, 4):
                return n
            raise Unhandled("ICCBased /N")
        if fam == "/DeviceN":
            names = doc.val(cs[1]) if len(cs) > 1 else None
            if isinstance(names, list) and names:
                return len(names)
            raise Unhandled("DeviceN names")
        if fam in COMPONENTS:
            return COMPONENTS[fam]
    raise Unhandled("colour space")


def sample(data, row_bytes, x, y, n, bpc, c):
    bit = (x * n + c) * bpc
    i = y * row_bytes + bit // 8
    if bpc == 8:
        return data[i]
    if bpc == 16:
        return data[i] << 8 | data[i + 1]
    return (data[i] >> (8 - bpc - bit % 8)) & ((1 << bpc) - 1)


def cells_in_rect(ctm, w, h, rect):
    """Sample cells (x, y) whose page-space area overlaps the rect's
    interior; the CTM axis-aligned or a quarter turn, so a column's page
    extent depends on x only and a row's on y only."""
    a, b, c, d, e, f = ctm
    if b == 0 and c == 0 and a != 0 and d != 0:
        ux, vy = (a, e, 0), (d, f, 1)      # u -> page x, v -> page y
    elif a == 0 and d == 0 and b != 0 and c != 0:
        ux, vy = (b, f, 1), (c, e, 0)      # u -> page y, v -> page x
    else:
        raise Unhandled("an image placed rotated or sheared")
    lo, hi = (rect[0], rect[1]), (rect[2], rect[3])

    def hit(scale, off, axis, t0, t1):
        p0, p1 = sorted((scale * t0 + off, scale * t1 + off))
        return p0 < hi[axis] - EPS and p1 > lo[axis] + EPS
    xs = [x for x in range(w) if hit(*ux, x / w, (x + 1) / w)]
    ys = [y for y in range(h) if hit(*vy, 1 - (y + 1) / h, 1 - y / h)]
    return [(x, y) for y in ys for x in xs]


def meets(ctm, rect):
    a, b, c, d, e, f = ctm
    pts = [(a * u + c * v + e, b * u + d * v + f) for u in (0, 1) for v in (0, 1)]
    return (min(p[0] for p in pts) < rect[2] - EPS and max(p[0] for p in pts) > rect[0] + EPS
            and min(p[1] for p in pts) < rect[3] - EPS and max(p[1] for p in pts) > rect[1] + EPS)


def image_fields(doc, d):
    w, h = doc.val(d.get("/Width")), doc.val(d.get("/Height"))
    if not (isinstance(w, int) and isinstance(h, int) and w > 0 and h > 0):
        raise Unhandled("image size")
    if w * h > 50_000_000:
        raise Unhandled("image too large to check")
    if d.get("/Filter") is not None:
        raise Unhandled(f"a filter qpdf did not remove: {d.get('/Filter')}")
    for k in ("/BPC", "/CS", "/D", "/DP", "/F", "/H", "/IM", "/I", "/W"):
        if k in d:
            raise Unhandled(f"inline-image abbreviation {k} on an image XObject")
    return w, h


def check_image(doc, ref, ctm, res, rect):
    d, _ = doc.stream(ref)
    w, h = image_fields(doc, d)
    data = doc.data(ref)
    cells = cells_in_rect(ctm, w, h, rect)
    if not cells:
        return 0
    stencil = doc.val(d.get("/ImageMask"))
    if stencil is True:
        dec = doc.val(d.get("/Decode"))
        if dec is None or [doc.val(x) for x in dec] == [0, 1]:
            want = 1
        elif [doc.val(x) for x in dec] == [1, 0]:
            want = 0
        else:
            raise Unhandled("image mask /Decode")
        bpc, n = 1, 1
    elif stencil not in (None, False):
        raise Unhandled("/ImageMask not a boolean")
    else:
        bpc = doc.val(d.get("/BitsPerComponent"))
        if bpc not in (1, 2, 4, 8, 16):
            raise Unhandled("bits per component")
        n = components(doc, d.get("/ColorSpace"), res)
        want = 0
        if d.get("/Mask") is not None:
            raise Unhandled("a /Mask")
    row = (w * n * bpc + 7) // 8
    if len(data) < row * h:
        raise Unhandled("short image data")
    bad = sum(1 for x, y in cells if any(sample(data, row, x, y, n, bpc, c) != want for c in range(n)))
    if bad:
        raise Residue(f"{bad} sample(s) of image {ref} under the rect not at {want}")
    sm = d.get("/SMask")
    if sm is not None and stencil is not True:
        sd, _ = doc.stream(sm)
        if sd is None:
            raise Unhandled("soft mask not a stream")
        sw, sh_ = image_fields(doc, sd)
        if doc.val(sd.get("/BitsPerComponent")) != 8 or sd.get("/Decode") is not None or sd.get("/Matte") is not None:
            raise Unhandled("soft mask format")
        sdata = doc.data(sm)
        if len(sdata) < sw * sh_:
            raise Unhandled("short soft mask data")
        sbad = sum(1 for x, y in cells_in_rect(ctm, sw, sh_, rect) if sdata[y * sw + x] != 255)
        if sbad:
            raise Residue(f"{sbad} soft-mask sample(s) of image {ref} under the rect not opaque")
    return len(cells)


def digests(doc, ref):
    d, f = doc.stream(ref)
    out = set()
    with open(f, "rb") as fh:
        out.add(hashlib.sha256(fh.read()).hexdigest())
    return out


def page_images(doc, page_no):
    pages = doc.j["pages"]
    if not 1 <= page_no <= len(pages):
        raise Unhandled("no such page")
    ref = pages[page_no - 1]["object"]
    return ref, walk(doc, ref)


def check(inp, out, page_no, rect, elsewhere=False):
    with tempfile.TemporaryDirectory() as tmp:
        try:
            dout = Doc(qpdf_json(out, tmp, "output"))
            pref, drawn = page_images(dout, page_no)
            checked = 0
            for ref, ctm, res in drawn:
                if meets(ctm, rect):
                    checked += check_image(dout, ref, ctm, res, rect)
            # The originals: images the input page drew, not carried.
            din = Doc(qpdf_json(inp, tmp, "input"))
            _, drawn_in = page_images(din, page_no)
            orig = set()
            for ref, ctm, _res in drawn_in:
                if meets(ctm, rect):
                    orig |= digests(din, ref)
            held = set()
            for k, o in dout.objs.items():
                if "stream" in o and o["stream"].get("datafile"):
                    with open(o["stream"]["datafile"], "rb") as fh:
                        held.add(hashlib.sha256(fh.read()).hexdigest())
            carried = orig & held
            if carried and not elsewhere:
                raise Residue(f"{len(carried)} original image(s) under the rect still carried")
            return "zero", checked
        except Residue as e:
            return f"residue:{e}", None
        except Unhandled as e:
            return f"unhandled:{e}", None


def main(argv):
    a = [x for x in argv if x != "--elsewhere"]
    if len(a) != 7:
        sys.exit(__doc__)
    verdict, _ = check(a[0], a[1], int(a[2]), [float(v) for v in a[3:7]], "--elsewhere" in argv)
    print(verdict)
    return 0 if verdict == "zero" else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
