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

 1. qpdf JSON v2 of each file, twice: every losslessly filtered stream
    decoded (`--decode-level=specialized`: a DCT, JPX, JBIG2 or CCITT
    image stays encoded and is `unhandled` — a lossy re-encoding of zero
    is not 0), and every stream as stored. qpdf repairing either file
    (exit 3), or failing, is `unhandled`.
 2. The output page's content and, recursively, the forms it draws are
    read with a tokenizer of their own: `q`/`Q`, `cm`, `Do`, a form's
    `/Matrix` and `/Resources` (a form without its own: `unhandled`).
    Every image an invocation draws gets its CTM. Clips are ignored (a
    clip hides ink, not data: a sample under the rect counts wherever a
    clip is). An inline image, a pattern (colour or resource), a Type 3
    font, a graphics state with a soft mask or a font, an image with
    /Alternates or /OPI, an annotation of the page with an appearance:
    `unhandled` — samples there are not followed.
 3. Geometry is exact: the numbers of the file and of the rect are read
    as the decimals they are written as (no floating point), so a cell
    whose edge lies on the rect's edge does not meet it and one inside
    by any amount does. For each image drawn whose unit square meets the
    rect's interior: its decoded samples, the CTM axis-aligned or a
    quarter turn (else `unhandled`). Every sample whose cell meets the
    rect's interior must hold the value that does not depend on the
    original: 0 in every component for a colour image (whatever /Decode
    or palette makes of it), and for an image mask (/ImageMask true) the
    value that paints nothing (1 under /Decode absent or [0 1], 0 under
    [1 0]; another /Decode: `unhandled`). Its /SMask, if any, must be 255
    (opaque) at those samples, at the mask's own grid; a /Mask:
    `unhandled`. The colour space is a full family name or one of the
    resources (an inline-image abbreviation is not one, in an XObject).
 4. No stream of the output may be an exact copy — as stored or as
    decoded — of an image the input page drew meeting the rect, or of
    its /SMask or /Mask. With `--elsewhere` (the redaction reported an
    original still drawn on another page) a copy is allowed only when
    another page of the output draws it, which is checked. A copy
    re-encoded differently is not detected by this step: the guarantee
    of step 3 is about what the page draws. The output must be a single
    revision (one `startxref`: an incremental update keeps earlier
    revisions qpdf's object table does not show; `unhandled`), and its
    raw bytes may not contain an original's bytes (32 or more) outside
    the objects judged above. An original with /Alternates or /OPI:
    `unhandled` (its other representations are not inventoried).
    A form with /Ref, a content stream with external data (/F) or a
    filter qpdf left: `unhandled`, on either file.

Verdict on stdout: `zero:<n>` (n samples under the rect, all at their
value); `no-samples` (no image sample meets the rect: nothing to settle);
`residue:<what>`; `unhandled:<why>`. Exit 0 only for `zero`. Only `zero`
settles the false signal.
"""
import hashlib
import json
import math
import os
import re
import subprocess
import sys
import tempfile
from fractions import Fraction

TIMEOUT = 300


class Unhandled(Exception):
    pass


class Residue(Exception):
    pass


def qpdf_json(pdf, tmp, tag, level="specialized"):
    j = os.path.join(tmp, f"{tag}-{level}.json")
    try:
        r = subprocess.run(["qpdf", "--json=2", "--json-stream-data=file",
                            "--json-stream-prefix=" + os.path.join(tmp, f"{tag}-{level}"),
                            f"--decode-level={level}", pdf, j], capture_output=True, timeout=TIMEOUT)
    except subprocess.TimeoutExpired:
        raise Unhandled(f"qpdf timed out on the {tag}")
    if r.returncode != 0:
        raise Unhandled(f"qpdf exit {r.returncode} on the {tag} (repaired or unreadable)")
    with open(j) as fh:
        # Exact numbers: a real as the decimal qpdf writes.
        return json.load(fh, parse_float=Fraction)


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

    def content(self, ref):
        """A content stream's bytes as a reader draws them: no external
        data (/F) and no filter qpdf left in place."""
        d, _ = self.stream(ref)
        if d is None:
            raise Unhandled(f"content {ref} is not a stream")
        if d.get("/F") is not None or d.get("/FFilter") is not None:
            raise Unhandled("a content stream with external data (/F)")
        if d.get("/Filter") is not None:
            raise Unhandled(f"a content stream filter qpdf did not remove: {d.get('/Filter')}")
        return self.data(ref)

    def data(self, ref):
        d, f = self.stream(ref)
        if f is None:
            raise Unhandled(f"stream {ref} has no data")
        with open(f, "rb") as fh:
            return fh.read()


def num(v):
    if isinstance(v, bool) or not isinstance(v, (int, Fraction)):
        raise Unhandled("a number expected")
    return Fraction(v)


# ---- content tokenizer -------------------------------------------------

DELIM = b"()<>[]{}/%"
WS = b" \t\r\n\f\x00"


def tokens(data):
    """(kind, value): ('op', b'cm'), ('num', Fraction), ('name', b'Im0'),
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
                yield "num", Fraction(word.decode())
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
            res = doc.val(o["/Resources"])
            if res is not None and not isinstance(res, dict):
                raise Unhandled("page resources")
            return res or {}
        node = o.get("/Parent")
    return {}


def walk(doc, page_ref):
    """[(image ref, ctm, resources)] the page's content draws, through forms."""
    page = doc.val(page_ref)
    if page.get("/Annots"):
        for a in doc.val(page["/Annots"]) or []:
            a = doc.val(a) or {}
            if a.get("/AP") is not None:
                raise Unhandled("an annotation with an appearance on the page")
    contents = page.get("/Contents")
    if isinstance(contents, str) and doc.stream(contents)[0] is None:
        contents = doc.val(contents)  # a reference to the array of streams
    refs = contents if isinstance(contents, list) else ([contents] if contents else [])
    data = b"\n".join(doc.content(r) for r in refs)
    out = []
    run(doc, data, resources_of_page(doc, page_ref), (1, 0, 0, 1, 0, 0), out, [], 0)
    return out


def check_resources(doc, res):
    if doc.val(res.get("/Pattern")):
        raise Unhandled("pattern resources (samples there are not followed)")
    for gs in (doc.val(res.get("/ExtGState")) or {}).values():
        gs = doc.val(gs) or {}
        sm = gs.get("/SMask")
        if sm is not None and doc.val(sm) != "/None":
            raise Unhandled("a graphics-state soft mask")
        if gs.get("/Font") is not None:
            raise Unhandled("a graphics-state font")
    for f in (doc.val(res.get("/Font")) or {}).values():
        if doc.val((doc.val(f) or {}).get("/Subtype")) == "/Type3":
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
                    if d.get("/Ref") is not None:
                        # A reference XObject: a reader may draw another
                        # file's page instead of these bytes.
                        raise Unhandled("a reference form (/Ref)")
                    if xo in chain:
                        raise Unhandled("a form drawing itself")
                    m = doc.val(d.get("/Matrix"))
                    m = [1, 0, 0, 1, 0, 0] if m is None else m
                    if not isinstance(m, list) or len(m) != 6:
                        raise Unhandled("form /Matrix")
                    m = tuple(num(doc.val(x)) for x in m)
                    fres = doc.val(d.get("/Resources"))
                    if not isinstance(fres, dict):
                        # Older files leave a form's resources to the page
                        # (ISO 32000 §7.8.3); which it is, is not settled here.
                        raise Unhandled("a form without its own resources")
                    run(doc, doc.content(xo), fres, mul(m, ctm), out, chain + [xo], depth + 1)
                else:
                    raise Unhandled(f"XObject subtype {sub}")
            elif v in (b"scn", b"SCN") and operands and operands[-1][0] == "name":
                raise Unhandled("a pattern colour")
            operands = []
        else:
            operands.append((kind, v))


# ---- samples -----------------------------------------------------------

# Full family names only: in an image XObject /G, /RGB, /I... are not
# colour spaces but names of the resources (abbreviations are inline-image
# syntax).
DEVICE = {"/DeviceGray": 1, "/DeviceRGB": 3, "/DeviceCMYK": 4}
FAMILY = {"/CalGray": 1, "/CalRGB": 3, "/Lab": 3, "/Indexed": 1, "/Separation": 1}


def components(doc, cs, res, depth=0):
    if depth > 8:
        raise Unhandled("colour space chain")
    cs = doc.val(cs)
    if isinstance(cs, str):
        if cs in DEVICE:
            return DEVICE[cs]
        named = (doc.val(res.get("/ColorSpace")) or {}).get(cs)
        if named is None:
            raise Unhandled(f"colour space {cs}")
        return components(doc, named, res, depth + 1)
    if isinstance(cs, list) and cs:
        fam = doc.val(cs[0])
        if fam == "/ICCBased":
            d, _ = doc.stream(cs[1]) if len(cs) > 1 else (None, None)
            n = doc.val((d or {}).get("/N"))
            if n in (1, 3, 4) and not isinstance(n, bool):
                return n
            raise Unhandled("ICCBased /N")
        if fam == "/DeviceN":
            names = doc.val(cs[1]) if len(cs) > 1 else None
            if isinstance(names, list) and names:
                return len(names)
            raise Unhandled("DeviceN names")
        if fam in FAMILY:
            return FAMILY[fam]
        if fam in DEVICE and len(cs) == 1:
            return DEVICE[fam]
    raise Unhandled("colour space")


def sample(data, row_bytes, x, y, n, bpc, c):
    bit = (x * n + c) * bpc
    i = y * row_bytes + bit // 8
    if bpc == 8:
        return data[i]
    if bpc == 16:
        return data[i] << 8 | data[i + 1]
    return (data[i] >> (8 - bpc - bit % 8)) & ((1 << bpc) - 1)


def axes(ctm):
    """(u's page axis, scale, offset), (v's ...): the CTM axis-aligned or
    a quarter turn."""
    a, b, c, d, e, f = ctm
    if b == 0 and c == 0 and a != 0 and d != 0:
        return (0, a, e), (1, d, f)
    if a == 0 and d == 0 and b != 0 and c != 0:
        return (1, b, f), (0, c, e)
    raise Unhandled("an image placed rotated or sheared")


def overlaps(axis, scale, off, t0, t1, rect):
    p0, p1 = sorted((scale * t0 + off, scale * t1 + off))
    return p0 < rect[axis + 2] and p1 > rect[axis]


def cells_in_rect(ctm, w, h, rect):
    """Sample cells (x, y) whose page-space area meets the rect's interior
    (exact arithmetic): a column's page extent depends on x only and a
    row's on y only."""
    ctm = tuple(Fraction(v) for v in ctm)
    rect = [Fraction(v) for v in rect]
    ux, vy = axes(ctm)
    xs = [x for x in range(w) if overlaps(*ux, Fraction(x, w), Fraction(x + 1, w), rect)]
    ys = [y for y in range(h) if overlaps(*vy, 1 - Fraction(y + 1, h), 1 - Fraction(y, h), rect)]
    return [(x, y) for y in ys for x in xs]


def meets(ctm, rect):
    """The unit square's page-space bounding box meets the rect's interior
    (a superset of the cells for every CTM)."""
    a, b, c, d, e, f = (Fraction(v) for v in ctm)
    rect = [Fraction(v) for v in rect]
    pts = [(a * u + c * v + e, b * u + d * v + f) for u in (0, 1) for v in (0, 1)]
    return (min(p[0] for p in pts) < rect[2] and max(p[0] for p in pts) > rect[0]
            and min(p[1] for p in pts) < rect[3] and max(p[1] for p in pts) > rect[1])


def image_fields(doc, d):
    w, h = doc.val(d.get("/Width")), doc.val(d.get("/Height"))
    if not (type(w) is int and type(h) is int and w > 0 and h > 0):
        raise Unhandled("image size")
    if w * h > 50_000_000:
        raise Unhandled("image too large to check")
    if d.get("/Filter") is not None:
        raise Unhandled(f"a filter qpdf did not remove: {d.get('/Filter')}")
    for k in ("/BPC", "/CS", "/D", "/DP", "/F", "/H", "/IM", "/I", "/W"):
        if k in d:
            raise Unhandled(f"inline-image abbreviation {k} on an image XObject")
    for k in ("/Alternates", "/OPI"):
        if d.get(k) is not None:
            raise Unhandled(f"an image with {k}")
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
        dec = None if dec is None else [doc.val(x) for x in dec]
        if dec is None or dec == [0, 1]:
            want = 1
        elif dec == [1, 0]:
            want = 0
        else:
            raise Unhandled("image mask /Decode")
        bpc = doc.val(d.get("/BitsPerComponent"))
        if bpc not in (None, 1):
            raise Unhandled("image mask bits per component")
        if d.get("/SMask") is not None or d.get("/Mask") is not None:
            raise Unhandled("an image mask with a mask")
        bpc, n = 1, 1
    elif stencil not in (None, False):
        raise Unhandled("/ImageMask not a boolean")
    else:
        bpc = doc.val(d.get("/BitsPerComponent"))
        if bpc not in (1, 2, 4, 8, 16) or isinstance(bpc, bool):
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
    if sm is not None:
        sd, _ = doc.stream(sm)
        if sd is None:
            raise Unhandled("soft mask not a stream")
        sw, sh_ = image_fields(doc, sd)
        if (doc.val(sd.get("/BitsPerComponent")) != 8 or sd.get("/Decode") is not None
                or sd.get("/Matte") is not None or doc.val(sd.get("/ColorSpace")) not in (None, "/DeviceGray")):
            raise Unhandled("soft mask format")
        sdata = doc.data(sm)
        if len(sdata) < sw * sh_:
            raise Unhandled("short soft mask data")
        sbad = sum(1 for x, y in cells_in_rect(ctm, sw, sh_, rect) if sdata[y * sw + x] != 255)
        if sbad:
            raise Residue(f"{sbad} soft-mask sample(s) of image {ref} under the rect not opaque")
    return len(cells)


def page_ref(doc, page_no):
    pages = doc.j["pages"]
    if not 1 <= page_no <= len(pages):
        raise Unhandled("no such page")
    return pages[page_no - 1]["object"]


def digest_file(f):
    with open(f, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def stream_digests(docs):
    """{digest: {object ref}} of every stream, in every representation."""
    out = {}
    for doc in docs:
        for k, o in doc.objs.items():
            if k.startswith("obj:") and "stream" in o and o["stream"].get("datafile"):
                out.setdefault(digest_file(o["stream"]["datafile"]), set()).add(k[4:])
    return out


def originals(docs, refs):
    """{digest: bytes} of the streams refs, their /SMask and /Mask streams,
    in every representation."""
    out = {}
    for doc in docs:
        for ref in refs:
            d, f = doc.stream(ref)
            files = [f] + [doc.stream((d or {}).get(k))[1] for k in ("/SMask", "/Mask")]
            for x in files:
                if x:
                    with open(x, "rb") as fh:
                        b = fh.read()
                    out[hashlib.sha256(b).hexdigest()] = b
    return out


def revisions(pdf):
    """startxref keywords in the file: more than one is an incremental
    update, whose earlier revisions qpdf's object table does not show."""
    with open(pdf, "rb") as fh:
        return len(re.findall(rb"(?<![A-Za-z])startxref(?![A-Za-z])", fh.read()))


def valid_rect(rect):
    if len(rect) != 4 or not all(isinstance(v, (int, float, Fraction)) and math.isfinite(v) for v in rect):
        raise Unhandled("rect not four finite numbers")
    if not (rect[0] < rect[2] and rect[1] < rect[3]):
        raise Unhandled("rect not ordered")
    return [Fraction(v) if not isinstance(v, float) else Fraction(repr(v)) for v in rect]


def check(inp, out, page_no, rect, elsewhere=False):
    """(verdict, samples checked)."""
    with tempfile.TemporaryDirectory() as tmp:
        try:
            rect = valid_rect(rect)
            dout = Doc(qpdf_json(out, tmp, "output"))
            if revisions(out) != 1:
                raise Unhandled("the output is not a single revision (incremental update or no startxref)")
            pref = page_ref(dout, page_no)
            checked = 0
            for ref, ctm, res in walk(dout, pref):
                if meets(ctm, rect):
                    checked += check_image(dout, ref, ctm, res, rect)
            # The originals: images the input page drew meeting the rect,
            # exact copies of which the output may not carry.
            din = Doc(qpdf_json(inp, tmp, "input"))
            din_raw = Doc(qpdf_json(inp, tmp, "input", "none"))
            refs = {ref for ref, ctm, _ in walk(din, page_ref(din, page_no)) if meets(ctm, rect)}
            for ref in refs:
                # An original with other representations: not inventoried.
                d, _ = din.stream(ref)
                for k in ("/Alternates", "/OPI"):
                    if d.get(k) is not None:
                        raise Unhandled(f"an input image with {k}")
            orig = originals((din, din_raw), refs)
            held = stream_digests((dout, Doc(qpdf_json(out, tmp, "output", "none"))))
            carried = set().union(*(held[h] for h in orig.keys() & held.keys()))
            # The file's bytes too, beyond the object table: an original
            # stored as it was in the input, wherever it lies. (An exact
            # copy listed in the table is judged below instead.)
            with open(out, "rb") as fh:
                whole = fh.read()
            loose = [h for h, b in orig.items() if h not in held and len(b) >= 32 and b in whole]
            if loose:
                raise Residue(f"{len(loose)} original image stream(s) in the output's bytes outside its objects")
            if carried:
                if not elsewhere:
                    raise Residue(f"{len(carried)} original image stream(s) under the rect still carried")
                drawn = set()
                for i, p in enumerate(dout.j["pages"], 1):
                    if i != page_no:
                        for ref, _, _ in walk(dout, p["object"]):
                            d, _ = dout.stream(ref)
                            drawn |= {ref} | {x for x in (d.get("/SMask"), d.get("/Mask")) if isinstance(x, str)}
                if carried - drawn:
                    raise Residue(f"{len(carried - drawn)} original image stream(s) carried, "
                                  "drawn by no other page")
            if not checked:
                return "no-samples", 0
            return f"zero:{checked}", checked
        except Residue as e:
            return f"residue:{e}", None
        except Unhandled as e:
            return f"unhandled:{e}", None


def main(argv):
    a = [x for x in argv if x != "--elsewhere"]
    if len(a) != 7:
        sys.exit(__doc__)
    try:
        rect = [float(v) for v in a[3:7]]
        page = int(a[2])
    except ValueError:
        sys.exit(__doc__)
    # The rect as written on the command line, exactly.
    rect = [Fraction(v) for v in a[3:7]] if all(math.isfinite(v) for v in rect) else rect
    verdict, _ = check(a[0], a[1], page, rect, "--elsewhere" in argv)
    print(verdict)
    return 0 if verdict.startswith("zero:") else 1


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
