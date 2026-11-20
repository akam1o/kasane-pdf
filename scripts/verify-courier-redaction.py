#!/usr/bin/env python3
"""Courier redaction outputs read by Poppler, MuPDF and pdf.js.

    scripts/verify-courier-redaction.py <kasane-pdf> [--json OUT] [--negative]

Synthetic cases (committed here, no corpus): non-embedded Courier in the
four styles and Courier New as a TrueType font, with WinAnsi,
/Differences (Greek and Cyrillic, which Kasane Courier takes from
Liberation Mono) and /Widths, a secret word and public text on one line,
and rects just overlapping or just clear of a glyph's ink. Each is
redacted with `--embed-substitute-fonts`; the case says whether that
succeeds or is refused. For an output, and for each renderer:

  1. the embedded font is the expected Kasane Courier face — its
     /FontFile2 is exactly the bundled file (qpdf), named so (pdffonts);
  2. the renderer draws with that embedded program, not a font of its
     own: Poppler asks fontconfig for no font (FC_DEBUG trace), MuPDF
     traces the embedded name, pdf.js loads it with its file;
  3. no ink of the secret is left: inside the rect nothing is drawn,
     and the secret is in no renderer's text nor in the decoded file;
  4. the public text is kept, exactly (Greek and Cyrillic included);
  5. widths, positions and styles: away from the rect the renderer's
     page matches kasane's render of the input (Kasane Courier, the same
     advances), as kasane's render of the output does;
  6. the glyphs taken from Liberation Mono have ink where kasane draws
     them.

The two edge cases put a rect 0.05 pt into (edge-overlapping) or past
(edge-clear) the ink of KEEP's last letter, P — not the secret, which
they keep. Instead of checks 3 and 4, for each renderer:

  E1. the text is exactly the line without that P (overlapping) or the
      whole line (clear) — whitespace removed, nothing else forgiven;
  E2. the P's whole ink box (from kasane's glyph bounds of the input,
      not from the output) has ink in a control: the input redacted
      with a rect over nothing, the same Kasane Courier embedded;
  E3. overlapping: no ink is left anywhere in that box; clear: the box
      is drawn as in the control;
  E4. the neighbouring letters (the E before, the R after) are drawn as
      in the control, and check 5 leaves out the P's box only.

A renderer that is missing, fails or times out, or a check it cannot
make (no control, no box), is `unverified`, never a pass (exit 2).
Exit 0: every case as expected; 1: a failure.
--negative runs the same checks on deliberately broken outputs (another
font program, another style's, a program that does not decode, a rect
that misses the secret, one that covers public text, a file cut in half
that cannot be decoded; for the edge: the P taken out of the text but
still drawn, and a clear rect that removed the P) and passes only if
each is caught, by the check meant to catch it, in every renderer.
The JSON names cases, renderers and check results only.
"""
import argparse, hashlib, json, os, re, shutil, subprocess, sys, tempfile, zlib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PDFJS = os.path.join(ROOT, "tools", "pdfjs", "render.mjs")
DPI = 144
FACES = {s: os.path.join(ROOT, "fonts", "kasane-courier", f"KasaneCourier-{s}.ttf")
         for s in ("Regular", "Bold", "Italic", "BoldItalic")}
# The word each case redacts (synthetic).
TARGET = "REDACTME"
WIDTHS = "/FirstChar 32 /LastChar 255 /Widths [" + " ".join(["600"] * 224) + "]"
DIFFS = "<< /Type /Encoding /BaseEncoding /WinAnsiEncoding /Differences [128 /uni0416 /uni0414 /uni03B1 /uni03A9] >>"
# The line: public, the redacted word, public with Cyrillic and Greek (codes 128-131).
LINE = "KEEP " + TARGET + " AFTER \\200\\201\\202\\203"
PUBLIC = ["KEEP", "AFTER", "ЖДαΩ"]


def pdf_bytes(font):
    content = f"BT /F1 14 Tf 20 100 Td ({LINE}) Tj ET".encode("latin-1")
    objs = [b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 360 160] /Resources << /Font << /F1 5 0 R >> >> "
            b"/Contents 4 0 R >>", b"<< /Length %d >>\nstream\n" % len(content) + content + b"\nendstream",
            font.encode()]
    out = bytearray(b"%PDF-1.4\n")
    offs = []
    for i, o in enumerate(objs):
        offs.append(len(out))
        out += f"{i + 1} 0 obj\n".encode() + o + b"\nendobj\n"
    x = len(out)
    out += f"xref\n0 {len(objs) + 1}\n0000000000 65535 f \n".encode()
    out += b"".join(f"{o:010} 00000 n \n".encode() for o in offs)
    out += f"trailer\n<< /Size {len(objs) + 1} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n".encode()
    return bytes(out)


def type1(base, widths=False):
    return f"<< /Type /Font /Subtype /Type1 /BaseFont /{base} /Encoding {DIFFS}{' ' + WIDTHS if widths else ''} >>"


def truetype(base, weight):
    return (f"<< /Type /Font /Subtype /TrueType /BaseFont /{base} /Encoding {DIFFS} {WIDTHS} /FontDescriptor "
            f"<< /Type /FontDescriptor /FontName /{base} /Flags 32 /FontWeight {weight} /FontBBox [0 -300 600 900] "
            f"/ItalicAngle 0 /Ascent 800 /Descent -200 /CapHeight 600 /StemV 80 >> >>")


# (name, font dictionary, expected face, rect kind, expected outcome)
CASES = [
    ("courier-regular", type1("Courier"), "Regular", "word", "ok"),
    ("courier-bold", type1("Courier-Bold"), "Bold", "word", "ok"),
    ("courier-oblique", type1("Courier-Oblique", widths=True), "Italic", "word", "ok"),
    ("courier-boldoblique", type1("Courier-BoldOblique"), "BoldItalic", "word", "ok"),
    ("couriernew-truetype-bold", truetype("CourierNewPSMT", 700), "Bold", "word", "ok"),
    ("couriernew-truetype-italic", truetype("CourierNewPS-ItalicMT", 400), "Italic", "word", "ok"),
    ("edge-overlapping", type1("Courier"), "Regular", "edge-in", "ok"),
    ("edge-clear", type1("Courier"), "Regular", "edge-out", "ok"),
    # Not a Courier this mode embeds: refused, no output.
    ("symbol-refused", "<< /Type /Font /Subtype /Type1 /BaseFont /Symbol >>", None, "word", "refused"),
]


def run(argv, **kw):
    return subprocess.run(argv, capture_output=True, timeout=120, **kw)


def kasane_chars(kasane, pdf):
    r = run([kasane, "text", pdf, "1", "--format", "json"])
    if r.returncode:
        raise RuntimeError("kasane text failed")
    # The CLI's JSON writes `via` unquoted.
    t = re.sub(r'"via": ([A-Za-z]+)', r'"via": "\1"', r.stdout.decode())
    return json.loads(t)


def rect_for(chars, kind):
    """The rect in user space: over the secret word, or 0.05 pt into /
    past the ink of the K of KEEP's last letter (P)."""
    line = "".join(c["unicode"] or "?" for c in chars)
    s0 = line.index(TARGET)
    if kind == "word":
        a, b = chars[s0], chars[s0 + len(TARGET) - 1]
        return (a["origin"][0] + 0.5, 95.0, b["origin"][0] + b["advance"][0] - 0.5, 115.0)
    p = chars[3]["bounds"]  # the P of KEEP
    dx = -0.05 if kind == "edge-in" else 0.05
    return (p[2] + dx, p[1], p[2] + dx + 0.4, p[3])


# The line's characters (kasane's order, the space included) the edge
# checks name: the P of KEEP and its neighbours.
EDGE_P, EDGE_BEFORE, EDGE_AFTER = 3, 2, 5
# The line as the renderers' text gives it, whitespace removed.
LINE_TEXT = "KEEP" + TARGET + "AFTERЖДαΩ"
EDGE_TEXT = {"edge-in": "KEE" + TARGET + "AFTERЖДαΩ", "edge-out": LINE_TEXT}
# A rect over nothing on the page: the control keeps every glyph.
NOWHERE = (300.0, 10.0, 340.0, 20.0)


def png_gray(path):
    from PIL import Image
    return Image.open(path).convert("L")


def render(who, pdf, tmp, kasane):
    os.makedirs(tmp, exist_ok=True)
    base = os.path.join(tmp, who)
    if who == "kasane":
        r = run([kasane, "render", pdf, "1", "--output", base + ".png", "--dpi", str(DPI)])
        return png_gray(base + ".png") if r.returncode == 0 else None, ""
    if who == "poppler":
        r = run(["pdftoppm", "-r", str(DPI), "-gray", "-singlefile", pdf, base], env=dict(os.environ, FC_DEBUG="1"))
        img = png_gray(base + ".pgm") if r.returncode == 0 else None
        return img, (r.stdout + r.stderr).decode("utf-8", "replace")
    if who == "mupdf":
        r = run(["mutool", "draw", "-q", "-r", str(DPI), "-c", "gray", "-o", base + ".pgm", pdf, "1"])
        t = run(["mutool", "trace", pdf, "1"])
        img = png_gray(base + ".pgm") if r.returncode == 0 else None
        return img, t.stdout.decode("utf-8", "replace") if t.returncode == 0 else None
    r = run(["node", PDFJS, "render", pdf, str(DPI), base + ".ppm"], cwd=os.path.dirname(PDFJS))
    f = run(["node", PDFJS, "fonts", pdf], cwd=os.path.dirname(PDFJS))
    img = png_gray(base + ".ppm") if r.returncode == 0 else None
    return img, f.stdout.decode() if f.returncode == 0 else None


def text(who, pdf):
    argv = {"poppler": ["pdftotext", "-f", "1", "-l", "1", pdf, "-"],
            "mupdf": ["mutool", "draw", "-q", "-F", "txt", "-o", "-", pdf, "1"],
            "pdfjs": ["node", PDFJS, "text", pdf]}[who]
    r = run(argv, cwd=os.path.dirname(PDFJS))
    return re.sub(r"\s+", "", r.stdout.decode("utf-8", "replace")) if r.returncode == 0 else None


def box_px(rect, img):
    s = DPI / 72.0
    return (int(rect[0] * s), int(img.height - rect[3] * s), int(rect[2] * s + 0.999), int(img.height - rect[1] * s + 0.999))


def ink(img, box, pad=0):
    """Dark pixels in `box`, `pad` px in from its edges where it is wide
    enough (an edge rect is under a pixel wide: judged whole)."""
    x0, y0, x1, y1 = box
    px = pad if x1 - x0 > 2 * pad else 0
    py = pad if y1 - y0 > 2 * pad else 0
    crop = img.crop((x0 + px, y0 + py, max(x1 - px, x0 + px + 1), max(y1 - py, y0 + py + 1)))
    return sum(1 for v in crop.tobytes() if v < 160)


def differs(a, b, outside, margin=3, inside=None):
    """Share of the inked pixels outside `outside` (px box, `margin` px
    around it) — or, with `inside`, of those in that px box only — whose
    grey differs by more than 64 between a and b, both blurred by one
    pixel: the renderers' anti-aliasing differs, glyph shapes and places
    do not: the same embedded font gives 0 here, another one about 0.2."""
    from PIL import ImageFilter
    if a.size != b.size:
        return 1.0
    a, b = a.filter(ImageFilter.GaussianBlur(1)), b.filter(ImageFilter.GaussianBlur(1))
    pa, pb = a.load(), b.load()
    n = d = 0
    for y in range(0, a.height):
        for x in range(a.width):
            if inside is not None:
                if not (inside[0] <= x < inside[2] and inside[1] <= y < inside[3]):
                    continue
            elif outside is not None:
                x0, y0, x1, y1 = outside
                if x0 - margin <= x <= x1 + margin and y0 - margin <= y <= y1 + margin:
                    continue
            if pa[x, y] < 250 or pb[x, y] < 250:
                n += 1
                d += abs(pa[x, y] - pb[x, y]) > 64
    return d / n if n else 0.0


def glyph_box_px(bounds, img, grow=1):
    """A glyph's ink box (user space) in px, `grow` px around for the
    anti-aliasing; None when there is no box."""
    if not bounds:
        return None
    x0, y0, x1, y1 = box_px(bounds, img)
    return (max(x0 - grow, 0), max(y0 - grow, 0), min(x1 + grow, img.width), min(y1 + grow, img.height))


def edge_checks(kind, img, t, ctrl, boxes):
    """E1-E4 for one renderer (module docstring): True / False, or
    'unverified' where the control or a box is missing."""
    r = {"E1_text_exact": t == EDGE_TEXT[kind]}
    p, before, after = (glyph_box_px(b, img) for b in boxes)
    if ctrl is None or ctrl.size != img.size or None in (p, before, after):
        for k in ("E2_control_P_inked", "E3_P_box", "E4_neighbours_kept", "5_matches_kasane"):
            r[k] = "unverified"
        return r
    # A control whose box is blank would let any output pass.
    r["E2_control_P_inked"] = ink(ctrl, p) >= 40
    if kind == "edge-in":
        r["E3_P_box"] = ink(img, p) == 0
    else:
        r["E3_P_box"] = ink(img, p) >= 0.8 * ink(ctrl, p) and differs(img, ctrl, None, inside=p) < 0.05
    r["E4_neighbours_kept"] = all(ink(ctrl, b) >= 40 and differs(img, ctrl, None, inside=b) < 0.05
                                  for b in (before, after))
    return r


def qpdf_objects(argv):
    """qpdf's JSON objects, or None when qpdf cannot read the file."""
    r = run(["qpdf", "--json=2", *argv])
    if r.returncode not in (0, 3):
        return None
    try:
        return json.loads(r.stdout)["qpdf"][1]
    except (ValueError, KeyError, IndexError):
        return None


def embedded_program(pdf):
    """The /FontFile2 stream of the page's font, decoded (qpdf json)."""
    objs = qpdf_objects(["--json-stream-data=inline", "--decode-level=generalized", pdf, "-"])
    if objs is None:
        return None, None
    fonts = [v["value"] for k, v in objs.items() if isinstance(v.get("value"), dict) and v["value"].get("/Type") == "/Font"]
    if len(fonts) != 1:
        return None, None
    fd = objs.get("obj:" + fonts[0]["/FontDescriptor"], {}).get("value", {})
    ff = fd.get("/FontFile2")
    st = objs.get("obj:" + ff, {}).get("stream") if ff else None
    if not st or "data" not in st:
        return fonts[0].get("/BaseFont"), None
    import base64
    return fonts[0].get("/BaseFont"), base64.b64decode(st["data"])


def font_bbox(pdf):
    """The /FontBBox the output's font descriptor declares."""
    objs = qpdf_objects([pdf, "-"])
    for v in (objs or {}).values():
        val = v.get("value")
        if isinstance(val, dict) and val.get("/Type") == "/FontDescriptor":
            return val.get("/FontBBox")
    return None


def check_output(kasane, src, out, rect, face, tmp, edge=None):
    """{renderer or 'file': {check: True / False / 'unverified'}}. `edge`:
    the edge kind, for E1-E4 (module docstring) in place of 3 and 4."""
    res = {"file": {}}
    want = open(FACES[face], "rb").read()
    base, program = embedded_program(out)
    res["file"]["1_embedded_face"] = base == f"/KasaneCourier-{face}" and program == want
    raw = open(out, "rb").read()
    # A plaintext check of the synthetic word in the file and in its fully
    # decoded form (beside the renderers' text and ink checks): qpdf must
    # read and decode the whole file without a warning (a repaired, cut or
    # damaged file is not one kasane wrote) — otherwise the check fails
    # rather than passing on what was not read.
    q = run(["qpdf", "--qdf", "--object-streams=disable", "--decode-level=all", out, "-"])
    decoded = q.returncode == 0 and q.stdout.rstrip().endswith(b"%%EOF")
    res["file"]["3_file_decoded"] = decoded
    res["file"]["3_target_not_in_file"] = decoded and TARGET.encode() not in raw and TARGET.encode() not in q.stdout
    kin, _ = render("kasane", src, tmp + "/in", kasane)
    kout, _ = render("kasane", out, tmp + "/out", kasane)
    try:
        chars = kasane_chars(kasane, out)
    except RuntimeError:
        chars = []
    # kasane reads its own output back (an output it cannot read fails).
    res["file"]["0_kasane_reads_output"] = kout is not None and bool(chars)
    written_bbox = font_bbox(out)
    supplemented = [c["bounds"] for c in chars if c["unicode"] in ("Ж", "Д", "α", "Ω") and c["bounds"]]
    if edge:
        # The boxes from the input (kasane's glyph bounds there), and the
        # control: the input with nothing redacted, the face embedded.
        try:
            src_chars = kasane_chars(kasane, src)
            boxes = [src_chars[i]["bounds"] for i in (EDGE_P, EDGE_BEFORE, EDGE_AFTER)]
            if "".join(src_chars[i]["unicode"] or "?" for i in (EDGE_BEFORE, EDGE_P, EDGE_AFTER)) != "EPR":
                boxes = [None] * 3
        except (RuntimeError, IndexError, KeyError):
            boxes = [None] * 3
        ctrl_pdf = os.path.join(tmp, "edge-control.pdf")
        if os.path.exists(ctrl_pdf):
            os.remove(ctrl_pdf)
        outcome, _ = redact(kasane, src, ctrl_pdf, NOWHERE)
        name, program = embedded_program(ctrl_pdf) if outcome == "ok" else (None, None)
        # The control is good only with the same face embedded; one that
        # cannot be made or read is unverified, another face a failure.
        ctrl_ok = name == f"/KasaneCourier-{face}" and program == want
        res["file"]["E2_control_embeds_face"] = (True if ctrl_ok else "unverified" if program is None else False)
        kin_p = glyph_box_px(boxes[0], kin) if kin is not None else None
    for who in ("poppler", "mupdf", "pdfjs"):
        r = {}
        try:
            img, trace = render(who, out, os.path.join(tmp, who), kasane)
            t = text(who, out)
        except (subprocess.TimeoutExpired, OSError):
            img, trace, t = None, None, None
        if img is None or trace is None or t is None:
            res[who] = {"error": "unverified"}
            continue
        if who == "poppler":
            r["2_uses_embedded"] = "First font" not in trace and "Best score" not in trace
        elif who == "mupdf":
            r["2_uses_embedded"] = f'font="KasaneCourier-{face}"' in trace or f"KasaneCourier-{face}" in trace
        else:
            fonts = json.loads(trace)
            # Loaded from the file's program: not missing, not a system
            # font, not rejected, the descriptor's box.
            r["2_uses_embedded"] = bool(fonts) and all(
                not f["missingFile"] and not f["invalid"] and f["systemFont"] is None and f["mimetype"]
                and f["bbox"] == written_bbox for f in fonts)
        box = box_px(rect, img)
        if edge:
            try:
                ctrl, _ = render(who, ctrl_pdf, os.path.join(tmp, who + "-control"), kasane) if ctrl_ok else (None, None)
            except (subprocess.TimeoutExpired, OSError):
                ctrl = None
            r.update(edge_checks(edge, img, t, ctrl, boxes))
            # Away from the P's box (and the rect) only, the page as kasane
            # drew the input.
            if r.get("5_matches_kasane") != "unverified":
                p = glyph_box_px(boxes[0], img)
                u = (min(p[0], box[0]), min(p[1], box[1]), max(p[2], box[2]), max(p[3], box[3]))
                r["5_matches_kasane"] = kin is not None and differs(img, kin, u, margin=1) < 0.02
        else:
            r["3_no_ink_in_rect"] = ink(img, box, pad=2) == 0
            r["3_target_not_extracted"] = TARGET not in t
            # Every public word, in order.
            at = [t.find(p) for p in PUBLIC]
            r["4_public_kept"] = all(i >= 0 for i in at) and at == sorted(at)
            # Away from the rect, the page as kasane drew the input (its
            # substitute, the same font and advances): what was kept is drawn
            # alike, in the same places and styles.
            r["5_matches_kasane"] = kin is not None and differs(img, kin, box) < 0.02
        r["6_supplemented_drawn"] = len(supplemented) == 4 and all(ink(img, box_px(b, img)) > 0 for b in supplemented)
        res[who] = r
    if edge:
        res["file"]["5_kasane_unchanged_outside"] = (
            "unverified" if kin_p is None or kout is None else
            differs(kin, kout, (min(kin_p[0], box_px(rect, kout)[0]), kin_p[1],
                                max(kin_p[2], box_px(rect, kout)[2]), kin_p[3]), margin=1) < 0.02)
    else:
        res["file"]["5_kasane_unchanged_outside"] = (kin is not None and kout is not None
                                                     and differs(kin, kout, box_px(rect, kout)) < 0.02)
    return res


def redact(kasane, src, out, rect):
    r = run([kasane, "redact", src, "--page", "1", "--rect", *(f"{v:.3f}" for v in rect), "--embed-substitute-fonts",
             "--output", out])
    if r.returncode == 0 and os.path.exists(out):
        return "ok", r.stderr
    if r.returncode == 1 and b"error [" in r.stderr and not os.path.exists(out):
        return "refused", r.stderr
    return "crash", r.stderr


def run_case(kasane, case, tmp):
    name, font, face, kind, expect = case
    os.makedirs(tmp, exist_ok=True)
    src, out = os.path.join(tmp, "in.pdf"), os.path.join(tmp, "out.pdf")
    open(src, "wb").write(pdf_bytes(font))
    if os.path.exists(out):
        os.remove(out)
    rect = rect_for(kasane_chars(kasane, src), kind) if face else (60.0, 95.0, 160.0, 115.0)
    outcome, err = redact(kasane, src, out, rect)
    res = {"outcome": outcome, "expected": expect}
    if outcome == "refused":
        res["refusal_quotes_no_target"] = TARGET.encode() not in err
    if outcome != "ok" or expect != "ok":
        return res, None
    edge = kind if kind in ("edge-in", "edge-out") else None
    res["checks"] = check_output(kasane, src, out, rect, face, tmp, edge=edge)
    return res, (src, out, rect, face)


def failures(name, res, kind):
    f = []
    if res["outcome"] != res["expected"]:
        f.append(f"{name}: outcome {res['outcome']}, expected {res['expected']}")
    if res.get("refusal_quotes_no_target") is False:
        f.append(f"{name}: the refusal quotes the redacted word")
    for who, checks in (res.get("checks") or {}).items():
        if "error" in checks:
            continue
        for k, v in checks.items():
            # The edge cases keep the secret on purpose: the file's check
            # of its absence is not theirs (E1-E4 are).
            if kind in ("edge-in", "edge-out") and k == "3_target_not_in_file":
                continue
            if v is not True and v != "unverified":
                f.append(f"{name}: {who}: {k}")
    return f


def unverified(res):
    """Renderers that could not be run, and checks that could not be
    made (`who.check`)."""
    out = []
    for who, c in (res.get("checks") or {}).items():
        if "error" in c:
            out.append(who)
        else:
            out += [f"{who}.{k}" for k, v in c.items() if v == "unverified"]
    return out


def program_object(pdf):
    """The object number of the page font's /FontFile2 (qpdf json)."""
    r = run(["qpdf", "--json=2", pdf, "-"])
    objs = json.loads(r.stdout)["qpdf"][1]
    font = next(v["value"] for v in objs.values() if isinstance(v.get("value"), dict)
                and v["value"].get("/Type") == "/Font")
    fd = objs["obj:" + font["/FontDescriptor"]]["value"]
    return int(fd["/FontFile2"].split()[0])


def with_program(pdf, data, tmp):
    """`pdf` with its font program's bytes replaced by `data` (QDF,
    lengths and xref repaired by fix-qdf)."""
    n = program_object(pdf)
    q = run(["qpdf", "--qdf", "--object-streams=disable", "--decode-level=all", pdf, "-"]).stdout
    m = re.search(rb"\n%d 0 obj\n" % n, q)
    i = q.index(b"stream\n", m.end()) + 7
    j = q.index(b"endstream", i)
    p = os.path.join(tmp, "neg.qdf")
    open(p, "wb").write(q[:i] + data + b"\n" + q[j:])
    fixed = os.path.join(tmp, "neg.pdf")
    with open(fixed, "wb") as fh:
        subprocess.run(["fix-qdf", p], stdout=fh, check=True, timeout=120)
    return fixed


def page_content_object(pdf):
    """The object number of the page's (last) content stream (qpdf json)."""
    objs = qpdf_objects([pdf, "-"])
    page = next(v["value"] for v in objs.values() if isinstance(v.get("value"), dict)
                and v["value"].get("/Type") == "/Page")
    c = page["/Contents"]
    return int((c[-1] if isinstance(c, list) else c).split()[0])


def with_content_appended(pdf, extra, tmp):
    """`pdf` with `extra` drawn after the page's content (QDF, lengths
    and xref repaired by fix-qdf)."""
    n = page_content_object(pdf)
    q = run(["qpdf", "--qdf", "--object-streams=disable", "--decode-level=all", pdf, "-"]).stdout
    m = re.search(rb"\n%d 0 obj\n" % n, q)
    i = q.index(b"stream\n", m.end()) + 7
    j = q.index(b"endstream", i)
    body = q[i:j].rstrip(b"\n")
    p = os.path.join(tmp, "neg-content.qdf")
    open(p, "wb").write(q[:i] + body + b"\n" + extra + b"\n" + q[j:])
    fixed = os.path.join(tmp, "neg-content.pdf")
    with open(fixed, "wb") as fh:
        subprocess.run(["fix-qdf", p], stdout=fh, check=True, timeout=120)
    return fixed


def glyph_as_image(kasane, pdf, bounds, tmp):
    """Content drawing the ink kasane renders in `bounds` (user space) of
    `pdf` as a stencil image there: the glyph drawn, no text."""
    img, _ = render("kasane", pdf, os.path.join(tmp, "stencil"), kasane)
    x0, y0, x1, y1 = box_px(bounds, img)
    crop = img.crop((x0, y0, x1, y1))
    w, h = crop.size
    rows = []
    for y in range(h):
        bits = "".join("0" if crop.getpixel((x, y)) < 128 else "1" for x in range(w))
        bits += "0" * (-len(bits) % 8)
        rows.append("".join(f"{int(bits[k:k + 8], 2):02x}" for k in range(0, len(bits), 8)))
    s = 72.0 / DPI
    ux, uy = x0 * s, (img.height - y1) * s
    return (f"q 0 g {w * s:.4f} 0 0 {h * s:.4f} {ux:.4f} {uy:.4f} cm BI /IM true /W {w} /H {h} /BPC 1 "
            f"/F /AHx ID {''.join(rows)}> EI Q").encode()


def only_failing(c, readers, expected):
    """An edge control caught by the checks meant for it: in every
    renderer exactly those fail, every other check (and every file check
    but the secret's, which the edge cases keep) passes, none
    unverified."""
    if any(v is not True for k, v in c["file"].items() if k != "3_target_not_in_file"):
        return False
    for w in readers:
        if "error" in c[w] or not all(k in c[w] for k in expected):
            return False
        if any((v is not False) if k in expected else (v is not True) for k, v in c[w].items()):
            return False
    return True


def negative(kasane, tmp):
    """Broken outputs the checks must catch: ({control: caught}, the edge
    controls' check results by renderer)."""
    caught, detail = {}, {}
    res, (src, out, rect, face) = run_case(kasane, CASES[0], tmp)
    readers = ("poppler", "mupdf", "pdfjs")
    lib = open(os.path.join(ROOT, "fonts", "liberation", "LiberationMono-Regular.ttf"), "rb").read()
    c = check_output(kasane, src, with_program(out, lib, tmp), rect, face, tmp)
    caught["another_font_program"] = c["file"]["1_embedded_face"] is False and all(
        c[w].get("5_matches_kasane") is False for w in readers)
    c = check_output(kasane, src, with_program(out, open(FACES["Bold"], "rb").read(), tmp), rect, face, tmp)
    caught["another_style"] = c["file"]["1_embedded_face"] is False and all(
        c[w].get("5_matches_kasane") is False for w in readers)
    c = check_output(kasane, src, with_program(out, b"not a font program", tmp), rect, face, tmp)
    caught["undecodable_program"] = c["file"]["1_embedded_face"] is False and all(
        c[w].get("5_matches_kasane") is False or c[w].get("error") for w in readers)
    chars = kasane_chars(kasane, src)
    miss = (rect[0] + 200.0, rect[1], rect[2] + 200.0, rect[3])
    o = os.path.join(tmp, "miss.pdf")
    redact(kasane, src, o, miss)
    c = check_output(kasane, src, o, rect, face, tmp)
    caught["target_left"] = c["file"]["3_target_not_in_file"] is False and all(
        c[w].get("3_no_ink_in_rect") is False and c[w].get("3_target_not_extracted") is False for w in readers)
    o = os.path.join(tmp, "cover.pdf")
    redact(kasane, src, o, (chars[0]["origin"][0] + 0.5, rect[1], rect[2], rect[3]))
    c = check_output(kasane, src, o, rect, face, tmp)
    caught["public_lost"] = all(c[w].get("4_public_kept") is False for w in readers)
    # A file qpdf cannot decode whole: the byte check must not pass.
    o = os.path.join(tmp, "cut.pdf")
    data = open(out, "rb").read()
    open(o, "wb").write(data[: len(data) // 2])
    c = check_output(kasane, src, o, rect, face, tmp)
    caught["file_not_decoded"] = c["file"]["3_file_decoded"] is False and c["file"]["3_target_not_in_file"] is False
    # The edge. A P out of the text but still drawn: the overlapping
    # case's output with the P's ink (kasane's render of the input, as a
    # stencil image) put back. A readable file, the right face, the text
    # as it should be — only E3 (the P's box) is to catch it, in each
    # renderer.
    edge_in = next(c for c in CASES if c[3] == "edge-in")
    edge_out = next(c for c in CASES if c[3] == "edge-out")
    d = os.path.join(tmp, "edge-neg")
    os.makedirs(d, exist_ok=True)
    _, (src, out, rect, face) = run_case(kasane, edge_in, d)
    p_bounds = kasane_chars(kasane, src)[EDGE_P]["bounds"]
    drawn = with_content_appended(out, glyph_as_image(kasane, src, p_bounds, d), d)
    c = check_output(kasane, src, drawn, rect, face, d, edge="edge-in")
    detail["edge_P_drawn_not_extracted"] = c
    caught["edge_P_drawn_not_extracted"] = only_failing(c, readers, {"E3_P_box"})
    # A clear rect that removed the P: the overlapping case's output
    # judged as the clear case's. E1 and E3 are both to catch it.
    _, (src2, _, rect2, _) = run_case(kasane, edge_out, os.path.join(d, "out"))
    c = check_output(kasane, src2, out, rect2, face, os.path.join(d, "out"), edge="edge-out")
    detail["edge_clear_P_removed"] = c
    caught["edge_clear_P_removed"] = only_failing(c, readers, {"E1_text_exact", "E3_P_box"})
    return caught, detail


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("kasane")
    ap.add_argument("--json")
    ap.add_argument("--negative", action="store_true")
    a = ap.parse_args()
    missing = [t for t in ("pdftoppm", "pdftotext", "mutool", "node", "qpdf") if not shutil.which(t)]
    if missing:
        print(f"verify-courier-redaction: unverified, missing {', '.join(missing)}")
        return 2
    doc, fails, unver = {"cases": {}}, [], []
    with tempfile.TemporaryDirectory() as tmp:
        for case in CASES:
            res, _ = run_case(a.kasane, case, tmp)
            doc["cases"][case[0]] = res
            fails += failures(case[0], res, case[3])
            unver += [f"{case[0]}:{w}" for w in unverified(res)]
            checks = res.get("checks") or {}
            ok = sum(v is True for c in checks.values() for v in c.values())
            print(f"{case[0]:28} {res['outcome']:8} {ok} checks passed"
                  + (f", unverified {unverified(res)}" if unverified(res) else ""), flush=True)
        if a.negative:
            caught, detail = negative(a.kasane, tmp)
            doc["negative"] = caught
            doc["negative_edge_checks"] = detail
            for k, v in caught.items():
                print(f"negative {k:24} {'caught' if v else 'NOT CAUGHT'}")
                if not v:
                    fails.append(f"negative control not caught: {k}")
    for f in fails:
        print("FAIL:", f)
    for u in unver:
        print("UNVERIFIED:", u)
    if a.json:
        json.dump(dict(doc, failures=fails, unverified=unver), open(a.json, "w"), indent=1)
    if fails:
        return 1
    if unver:
        return 2
    print("verify-courier-redaction: passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
