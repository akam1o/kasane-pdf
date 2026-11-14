#!/usr/bin/env python3
"""External renderers on redaction outputs: Poppler, MuPDF and pdf.js.

    scripts/verify-renderers.py <kasane-pdf> <advisory dump dir> --json OUT
        [--baseline compat/renderers/baseline.json] [--write-baseline FILE] [--jobs N]

The cases are synthetic and committed (no corpus): every case of
scripts/verify-redact.py, every redaction input of the advisory
regressions (`KASANE_ADVISORY_DUMP=<dir> cargo test --release --test
redact_advisories`: F, G, H, I and the earlier ones), and the few cases
below for what neither covers (XFA, a shading, a tiling pattern, a
widget's appearance). Each is redacted with the CLI; an output is
rendered (page 1, 72 dpi, grey) and its text extracted by each renderer,
and compared with the input by the same renderer:

  * inside the rect (2 px in from its edges): a non-white pixel that is
    unchanged from the input is ink left in place — a **leak**, whatever
    else holds (not judged where an image lies inside the rect: zeroed
    samples are black by design, and scripts/verify-redact.py checks
    those bytes);
  * the case's secret in the extracted text is a **leak**;
  * the public text the renderer extracted from the input and not from
    the output is a **loss**;
  * outside the rect (2 px out): every changed pixel is classified —
    `antialias` (grey level moved by at most 48), `snap` (within 4 px of
    the rect: edge pixels), `font_substitution` (a case that embeds a
    substitute font), else `changed`. Only `changed` counts, and only
    against the baseline: more than the baseline records for the same
    case with the **same renderer version** is a regression; with
    another version the comparison is `incomparable` (not a pass —
    re-baseline with --write-baseline after checking the differences).

A leak or a loss fails regardless of the baseline; so does a crash (no
typed error), a refusal where the case expects success, or an outcome
different from the baseline. The JSON names cases, renderers, versions
and counts only (the cases are synthetic; no rectangles are written).
Exit 0 passed, 1 failed, 3 incomparable, 2 could not run.
"""
import argparse
import concurrent.futures
import importlib.util
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DPI = 72
RENDERERS = ("poppler", "mupdf", "pdfjs")
PDFJS = os.path.join(ROOT, "tools", "pdfjs", "render.mjs")
AA_DELTA = 48
SNAP_PX = 4


def load_verify_redact():
    spec = importlib.util.spec_from_file_location("verify_redact", os.path.join(ROOT, "scripts", "verify-redact.py"))
    mod = importlib.util.module_from_spec(spec)
    argv = sys.argv
    sys.argv = [argv[0]]
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.argv = argv
    return mod


def synthetic_cases(vr):
    """XFA, shading, tiling pattern and widget appearance across the rect."""
    pub = b"BT /F1 12 Tf 10 20 Td (PUBLICWORD) Tj ET"
    shading = b"<< /ShadingType 2 /ColorSpace /DeviceRGB /Coords [60 0 90 0] /Extend [true true] " \
              b"/Function << /FunctionType 2 /Domain [0 1] /C0 [1 0 0] /C1 [0 0 1] /N 1 >> >>"
    pattern = vr.stream(b"/Type /Pattern /PatternType 1 /PaintType 1 /TilingType 1 /BBox [0 0 4 4] "
                        b"/XStep 4 /YStep 4 /Resources << >>", b"0 0 1 rg 0 0 2 2 re f")
    xfa = vr.stream(b"", b"<xdp:xdp xmlns:xdp=\"http://ns.adobe.com/xdp/\"><field>SECRETTEXT</field></xdp:xdp>")
    ap = vr.stream(b"/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 5 0 R >> >>",
                   b"BT /F1 12 Tf 2 5 Td (SECRETTEXT) Tj ET")
    return [
        ("syn-shading", vr.build(vr.page(vr.TEXT + b" q 60 95 30 10 re W n /Sh0 sh Q",
                                         res=b"/Font << /F1 5 0 R >> /Shading << /Sh0 8 0 R >>",
                                         more={8: shading})), []),
        ("syn-tiling-pattern", vr.build(vr.page(vr.TEXT + b" /Pattern cs /P0 scn 20 95 160 10 re f",
                                                res=b"/Font << /F1 5 0 R >> /Pattern << /P0 8 0 R >>",
                                                more={8: pattern})), []),
        ("syn-xfa", vr.build(vr.page(vr.TEXT, cat=b"/AcroForm 8 0 R ",
                                     more={8: b"<< /Fields [] /XFA 9 0 R >>", 9: xfa})), []),
        ("syn-widget-appearance", vr.build(vr.page(pub, page_extra=b"/Annots [8 0 R]", cat=b"/AcroForm << /Fields [8 0 R] >> ",
                                                   more={8: b"<< /Type /Annot /Subtype /Widget /Rect [50 95 150 115] /FT /Tx "
                                                            b"/T (f) /V (SECRETTEXT) /AP << /N 9 0 R >> >>", 9: ap})), []),
    ]


def cases(dump):
    """(name, pdf bytes, redact args without --output, rect, secret text or
    None, public text or None, images inside, expected or None)."""
    vr = load_verify_redact()
    rect = [float(v) for v in vr.RECT]
    out = []
    for name, pdf, args, expect in vr.CASES:
        custom = "--rect" in args
        r = [float(v) for v in args[args.index("--rect") + 1:args.index("--rect") + 5]] if custom else rect
        argv = ["--page", "1"] + ([] if custom else ["--rect", *vr.RECT]) + list(args)
        # A case's own rect may cover the public text too (verify-redact
        # judges the public text with its default rect only).
        out.append((f"vr:{name}", pdf, argv, r, "SECRETTEXT", None if custom else "PUBLICWORD",
                    "image" in name or custom, expect))
    for name, pdf, args in synthetic_cases(vr):
        out.append((name, pdf, ["--page", "1", "--rect", *vr.RECT, *args], rect, "SECRETTEXT", "PUBLICWORD", False, None))
    for m in sorted(f for f in os.listdir(dump) if f.startswith("manifest-") and f.endswith(".tsv")):
        for line in open(os.path.join(dump, m)).read().splitlines():
            if not line.strip():
                continue
            file, _ghsa, cmds, expect = line.split("\t")
            cmd = next((c.split() for c in cmds.split("|") if c.startswith("redact ")), None)
            if cmd is None:
                continue
            argv = cmd[1:]
            i = argv.index("--rect")
            r = [float(v) for v in argv[i + 1:i + 5]]
            secret = None
            if expect.startswith("safe-text:"):
                secret = expect.split(":", 1)[1]
            elif expect.startswith("safe-bytes:") and not expect.startswith("safe-bytes:hex:"):
                secret = expect.split(":", 1)[1]
            stem = file[:-4]
            pdf = open(os.path.join(dump, file), "rb").read()
            out.append((f"adv:{stem}", pdf, argv, r, secret, None, "image" in stem,
                        "refused" if expect == "refused" else None))
    return out


def tool_versions():
    v = {}
    r = subprocess.run(["pdftoppm", "-v"], capture_output=True, text=True)
    m = re.search(r"pdftoppm version ([0-9.]+)", r.stdout + r.stderr)
    v["poppler"] = m.group(1) if m else None
    r = subprocess.run(["mutool", "-v"], capture_output=True, text=True)
    m = re.search(r"mutool version ([0-9.]+)", r.stdout + r.stderr)
    v["mupdf"] = m.group(1) if m else None
    r = subprocess.run(["node", PDFJS, "version"], capture_output=True, text=True, cwd=os.path.dirname(PDFJS))
    v["pdfjs"] = r.stdout.strip() if r.returncode == 0 and re.fullmatch(r"[0-9.]+", r.stdout.strip()) else None
    return v


def read_pnm(path):
    data = open(path, "rb").read()
    parts = data.split(maxsplit=4)
    kind, w, h = parts[0], int(parts[1]), int(parts[2])
    pix = parts[4][: w * h * (3 if kind == b"P6" else 1)]
    if kind == b"P6":
        pix = bytes((pix[i] * 299 + pix[i + 1] * 587 + pix[i + 2] * 114) // 1000 for i in range(0, len(pix), 3))
    return w, h, pix


def render(renderer, pdf, base):
    if renderer == "poppler":
        subprocess.run(["pdftoppm", "-r", str(DPI), "-gray", "-f", "1", "-l", "1", "-singlefile", pdf, base],
                       capture_output=True, check=True, timeout=120)
        return read_pnm(base + ".pgm")
    if renderer == "mupdf":
        subprocess.run(["mutool", "draw", "-q", "-r", str(DPI), "-c", "gray", "-o", base + ".pgm", pdf, "1"],
                       capture_output=True, check=True, timeout=120)
        return read_pnm(base + ".pgm")
    subprocess.run(["node", PDFJS, "render", pdf, str(DPI), base + ".ppm"], capture_output=True, check=True,
                   timeout=120, cwd=os.path.dirname(PDFJS))
    return read_pnm(base + ".ppm")


def text(renderer, pdf):
    if renderer == "poppler":
        argv = ["pdftotext", "-f", "1", "-l", "1", pdf, "-"]
    elif renderer == "mupdf":
        argv = ["mutool", "draw", "-q", "-F", "txt", "-o", "-", pdf, "1"]
    else:
        argv = ["node", PDFJS, "text", pdf]
    r = subprocess.run(argv, capture_output=True, timeout=120, cwd=os.path.dirname(PDFJS))
    return r.stdout.decode("utf-8", "replace") if r.returncode == 0 else None


def compare(a, b, rect, substitute):
    """Classify every pixel of output b against input a (same renderer)."""
    w, h, pa = a
    w1, h1, pb = b
    if (w, h) != (w1, h1):
        return None
    s = DPI / 72.0
    x0, x1 = rect[0] * s, rect[2] * s
    y0, y1 = h - rect[3] * s, h - rect[1] * s
    c = {"ink_left": 0, "antialias": 0, "snap": 0, "font_substitution": 0, "changed": 0}
    for y in range(h):
        row = y * w
        for x in range(w):
            i = row + x
            if x0 + 2 <= x <= x1 - 2 and y0 + 2 <= y <= y1 - 2:
                if pb[i] != 255 and pa[i] == pb[i]:
                    c["ink_left"] += 1
                continue
            if not (x < x0 - 2 or x > x1 + 2 or y < y0 - 2 or y > y1 + 2):
                continue
            d = abs(pa[i] - pb[i])
            if not d:
                continue
            if d <= AA_DELTA:
                c["antialias"] += 1
            elif x0 - SNAP_PX <= x <= x1 + SNAP_PX and y0 - SNAP_PX <= y <= y1 + SNAP_PX:
                c["snap"] += 1
            elif substitute:
                c["font_substitution"] += 1
            else:
                c["changed"] += 1
    return c


def norm(t):
    return re.sub(r"\s+", "", t or "")


def run_case(kasane, case, renderers):
    name, pdf, argv, rect, secret, public, images_inside, expect = case
    res = {"renderers": {}}
    with tempfile.TemporaryDirectory() as tmp:
        src, out = os.path.join(tmp, "in.pdf"), os.path.join(tmp, "out.pdf")
        open(src, "wb").write(pdf)
        try:
            r = subprocess.run([kasane, "redact", src, *argv, "--output", out], capture_output=True, timeout=300)
        except subprocess.TimeoutExpired:
            res["outcome"] = "timeout"
            return name, res
        if r.returncode == 0 and os.path.exists(out):
            res["outcome"] = "ok"
        elif r.returncode == 1 and b"error [" in r.stderr and not os.path.exists(out):
            res["outcome"] = "refused"
        else:
            res["outcome"] = "crash"
        res["expected"] = expect
        if res["outcome"] != "ok":
            return name, res
        substitute = "--embed-substitute-fonts" in argv or name.endswith("-embedded")
        for rd in renderers:
            got = {}
            try:
                c = compare(render(rd, src, os.path.join(tmp, f"{rd}-in")), render(rd, out, os.path.join(tmp, f"{rd}-out")),
                            rect, substitute)
            except (subprocess.CalledProcessError, subprocess.TimeoutExpired, ValueError, IndexError):
                got["error"] = "render_failed"
                res["renderers"][rd] = got
                continue
            if c is None:
                got["error"] = "size_changed"
                res["renderers"][rd] = got
                continue
            if images_inside:
                got["ink_left_not_judged"] = c.pop("ink_left")
            got.update(c)
            tin, tout = text(rd, src), text(rd, out)
            if tin is None or tout is None:
                # Without both texts neither the secret nor the public
                # text was judged: not a pass.
                got["error"] = "text_failed"
            if secret:
                got["secret_extracted"] = norm(secret) in norm(tout)
            if public:
                got["public_lost"] = norm(public) in norm(tin) and norm(public) not in norm(tout)
            res["renderers"][rd] = got
    return name, res


def judge(results, versions, baseline):
    """Problems (closed vocabulary: kind:case:renderer) and whether the
    baseline comparison could be made."""
    problems, incomparable = [], []
    base_cases = (baseline or {}).get("cases", {})
    base_tools = (baseline or {}).get("tools", {})
    for rd in RENDERERS:
        if baseline is not None and base_tools.get(rd) != versions.get(rd):
            incomparable.append(rd)
    if baseline is not None:
        for name in sorted(set(base_cases) - set(results)):
            problems.append(f"case_missing:{name}")
    for name, res in sorted(results.items()):
        out = res["outcome"]
        if out in ("crash", "timeout"):
            problems.append(f"{out}:{name}")
        exp = res.get("expected")
        if exp and out != exp:
            problems.append(f"outcome_not_expected:{name}")
        b = base_cases.get(name)
        if baseline is not None and b is None:
            problems.append(f"case_not_in_baseline:{name}")
        elif b is not None and b.get("outcome") != out:
            problems.append(f"outcome_changed:{name}")
        if out == "ok":
            for rd in RENDERERS:
                if rd not in res.get("renderers", {}):
                    problems.append(f"renderer_missing:{name}:{rd}")
        for rd, got in res.get("renderers", {}).items():
            if got.get("error"):
                problems.append(f"{got['error']}:{name}:{rd}")
                continue
            if got.get("ink_left"):
                problems.append(f"leak_ink:{name}:{rd}")
            if got.get("secret_extracted"):
                problems.append(f"leak_text:{name}:{rd}")
            if got.get("public_lost"):
                problems.append(f"loss_text:{name}:{rd}")
            if b is not None and rd not in incomparable:
                was = ((b.get("renderers") or {}).get(rd) or {}).get("changed")
                if was is None or got.get("changed", 0) > was:
                    problems.append(f"loss_drawing:{name}:{rd}")
    return problems, incomparable


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("kasane")
    ap.add_argument("dump")
    ap.add_argument("--json", required=True)
    ap.add_argument("--baseline", default=os.path.join(ROOT, "compat", "renderers", "baseline.json"))
    ap.add_argument("--write-baseline")
    ap.add_argument("--jobs", type=int, default=os.cpu_count() or 2)
    a = ap.parse_args()
    for tool in ("pdftoppm", "pdftotext", "mutool", "node"):
        if shutil.which(tool) is None:
            print(f"verify-renderers: {tool} not found")
            return 2
    versions = tool_versions()
    if not all(versions.values()):
        print(f"verify-renderers: a renderer did not report its version: {versions}")
        return 2
    all_cases = cases(a.dump)
    results = {}
    with concurrent.futures.ThreadPoolExecutor(a.jobs) as ex:
        for name, res in ex.map(lambda c: run_case(a.kasane, c, RENDERERS), all_cases):
            results[name] = res
    if versions != tool_versions():
        print("verify-renderers: a renderer's version changed during the run")
        return 2
    baseline = None
    if not a.write_baseline:
        if not os.path.exists(a.baseline):
            print("verify-renderers: no baseline")
            return 2
        baseline = json.load(open(a.baseline))
    problems, incomparable = judge(results, versions, baseline)
    hard = [p for p in problems]
    status = "failed" if hard else ("incomparable" if incomparable else "passed")
    outcomes = {}
    for r in results.values():
        outcomes[r["outcome"]] = outcomes.get(r["outcome"], 0) + 1
    summary = {
        "cases": len(results),
        "outcomes": outcomes,
        "renderer_runs": sum(len(r.get("renderers", {})) for r in results.values()),
        "leaks": sum(p.startswith("leak_") for p in problems),
        "losses": sum(p.startswith("loss_") for p in problems),
        "pixels": {k: sum(g.get(k, 0) for r in results.values() for g in r.get("renderers", {}).values())
                   for k in ("antialias", "snap", "font_substitution", "changed")},
    }
    doc = {"schema_version": 1, "tools": versions, "dpi": DPI, "status": status, "problems": problems,
           "incomparable": incomparable, "summary": summary, "cases": results}
    if a.write_baseline:
        doc["status"] = "baseline"
        for r in doc["cases"].values():
            r.pop("expected", None)
        with open(a.write_baseline, "w") as fh:
            json.dump({k: doc[k] for k in ("schema_version", "tools", "dpi", "cases")}, fh, indent=1, sort_keys=True)
            fh.write("\n")
    with open(a.json, "w") as fh:
        json.dump(doc, fh, indent=1, sort_keys=True)
        fh.write("\n")
    print(f"verify-renderers: {status}; {summary['cases']} cases, outcomes {outcomes}, "
          f"{summary['renderer_runs']} renderer comparisons, tools {versions}")
    for p in problems:
        print("  problem:", p)
    for rd in incomparable:
        print(f"  incomparable: {rd} {versions.get(rd)} (baseline {(baseline or {}).get('tools', {}).get(rd)})")
    if a.write_baseline:
        return 0 if not [p for p in problems if p.split(':')[0] in ('leak_ink', 'leak_text', 'loss_text', 'crash', 'timeout',
                                                                    'outcome_not_expected')] else 1
    return {"passed": 0, "failed": 1, "incomparable": 3}[status]


if __name__ == "__main__":
    sys.exit(main())
