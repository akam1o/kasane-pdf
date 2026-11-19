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

A renderer that is missing, fails or times out is `unverified`, never a
pass (exit 2). Exit 0: every case as expected; 1: a failure.
--negative runs the same checks on deliberately broken outputs (another
font program, another style's, a program that does not decode, a rect
that misses the secret, one that covers public text) and passes only if
each is caught.
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


def differs(a, b, outside):
    """Share of the inked pixels outside `outside` (px box) whose grey
    differs by more than 64 between a and b, both blurred by one pixel —
    the renderers' anti-aliasing differs, glyph shapes and places do not:
    the same embedded font gives 0 here, another one about 0.2."""
    from PIL import ImageFilter
    if a.size != b.size:
        return 1.0
    a, b = a.filter(ImageFilter.GaussianBlur(1)), b.filter(ImageFilter.GaussianBlur(1))
    pa, pb = a.load(), b.load()
    x0, y0, x1, y1 = outside
    n = d = 0
    for y in range(0, a.height):
        for x in range(a.width):
            if x0 - 3 <= x <= x1 + 3 and y0 - 3 <= y <= y1 + 3:
                continue
            if pa[x, y] < 250 or pb[x, y] < 250:
                n += 1
                d += abs(pa[x, y] - pb[x, y]) > 64
    return d / n if n else 0.0


def embedded_program(pdf):
    """The /FontFile2 stream of the page's font, decoded (qpdf json)."""
    r = run(["qpdf", "--json=2", "--json-stream-data=inline", "--decode-level=generalized", pdf, "-"])
    if r.returncode not in (0, 3):
        return None, None
    doc = json.loads(r.stdout)
    objs = doc["qpdf"][1]
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
    r = run(["qpdf", "--json=2", pdf, "-"])
    objs = json.loads(r.stdout)["qpdf"][1]
    for v in objs.values():
        val = v.get("value")
        if isinstance(val, dict) and val.get("/Type") == "/FontDescriptor":
            return val.get("/FontBBox")
    return None


def check_output(kasane, src, out, rect, face, tmp):
    """{renderer or 'file': {check: True / False / 'unverified'}}."""
    res = {"file": {}}
    want = open(FACES[face], "rb").read()
    base, program = embedded_program(out)
    res["file"]["1_embedded_face"] = base == f"/KasaneCourier-{face}" and program == want
    raw = open(out, "rb").read()
    dec = run(["qpdf", "--qdf", "--object-streams=disable", "--decode-level=all", out, "-"]).stdout
    res["file"]["3_target_not_in_file"] = TARGET.encode() not in raw and TARGET.encode() not in dec
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
        r["3_no_ink_in_rect"] = ink(img, box, pad=2) == 0
        r["3_target_not_extracted"] = TARGET not in t
        r["4_public_kept"] = all(p in t for p in PUBLIC)
        # Away from the rect, the page as kasane drew the input (its
        # substitute, the same font and advances): what was kept is drawn
        # alike, in the same places and styles.
        r["5_matches_kasane"] = kin is not None and differs(img, kin, box) < 0.02
        r["6_supplemented_drawn"] = len(supplemented) == 4 and all(ink(img, box_px(b, img)) > 0 for b in supplemented)
        res[who] = r
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
    res["checks"] = check_output(kasane, src, out, rect, face, tmp)
    edge_p = None
    if kind in ("edge-in", "edge-out"):
        t = text("poppler", out) or ""
        res["edge_P_removed"] = edge_p = not t.startswith("KEEP")
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
            if kind in ("edge-in", "edge-out") and k in ("3_target_not_extracted", "3_no_ink_in_rect",
                                                         "3_target_not_in_file", "4_public_kept"):
                continue
            if v is not True:
                f.append(f"{name}: {who}: {k}")
    if kind == "edge-in" and res.get("edge_P_removed") is not True:
        f.append(f"{name}: the P the rect overlaps is kept")
    if kind == "edge-out" and res.get("edge_P_removed") is not False:
        f.append(f"{name}: the P the rect is clear of is removed")
    return f


def unverified(res):
    return [who for who, c in (res.get("checks") or {}).items() if "error" in c]


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


def negative(kasane, tmp):
    """Broken outputs the checks must catch: {control: caught}."""
    caught = {}
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
    return caught


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
            caught = negative(a.kasane, tmp)
            doc["negative"] = caught
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
