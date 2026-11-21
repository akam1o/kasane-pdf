#!/usr/bin/env python3
"""Which font file Poppler draws each non-embedded Standard 14 name, and
its aliases, with in E0 (the system configuration) and E1
(setup-e1.sh's), and whether E1 changes only Courier.

    scripts/courier-reference/probe.py E1DIR --json OUT

For each probe font (a one-page synthetic PDF, the font not embedded)
`pdftoppm` runs with FC_DEBUG=1 and the file fontconfig matched for it is
read from its trace — not `fc-match`, which does not see the pattern
Poppler builds. Then the page is rendered in both environments.

Files are compared by content (SHA-256), not by path: E1 uses the
system's files, the isolated E2 (setup-e2.py) its own copies of them.
Fails (exit 1) unless: every Courier / Courier New probe draws, in E1,
the Nimbus Mono PS file of its style from E1DIR; every other probe
matches a file of the same content in E0 and E1 and renders the same
pixels; no probe in E0 matches a file of E1DIR; the system font list
(`fc-list`) is the same with and without E1 set up; E1's fonts are E0's
and the four Nimbus Mono PS faces, by content. Exit 2: a tool is
missing. (Whether a reference is isolated from the machine is
isolation-probe.py's to tell.)
"""
import argparse, hashlib, json, os, re, shutil, subprocess, sys, tempfile

COURIER = {  # base font: style of the Nimbus Mono PS file expected in E1
    "Courier": "Regular", "Courier-Bold": "Bold", "Courier-Oblique": "Italic",
    "Courier-BoldOblique": "BoldItalic",
    "CourierNewPSMT": "Regular", "CourierNewPS-BoldMT": "Bold", "CourierNewPS-ItalicMT": "Italic",
    "CourierNewPS-BoldItalicMT": "BoldItalic", "Courier New,Bold": "Bold",
}
OTHERS = ["Helvetica", "Helvetica-Bold", "Helvetica-Oblique", "Helvetica-BoldOblique", "Times-Roman",
          "Times-Bold", "Times-Italic", "Times-BoldItalic", "ArialMT", "Arial,Bold", "TimesNewRomanPSMT",
          "Symbol", "ZapfDingbats", "SomeTypewriter", "LucidaConsole", "Courierish", "CourierPrime",
          "Verdana", "Georgia"]


def probe_pdf(base):
    """A page of text in the non-embedded font `base` (a TrueType font
    with a descriptor for the names that are no Standard 14 name)."""
    std14 = base.split(",")[0] in ("Courier", "Courier-Bold", "Courier-Oblique", "Courier-BoldOblique", "Helvetica",
                                   "Helvetica-Bold", "Helvetica-Oblique", "Helvetica-BoldOblique", "Times-Roman",
                                   "Times-Bold", "Times-Italic", "Times-BoldItalic", "Symbol", "ZapfDingbats")
    name = base.replace(" ", "#20")
    if std14:
        font = f"<< /Type /Font /Subtype /Type1 /BaseFont /{name} >>"
    else:
        mono = base in ("SomeTypewriter", "LucidaConsole") or base.startswith("Courier")
        flags = 33 if mono else 32
        widths = " ".join(["600"] * 95 if mono else ["278"] + ["556", "500", "722", "333"] * 23 + ["556"] * 2)
        font = (f"<< /Type /Font /Subtype /TrueType /BaseFont /{name} /Encoding /WinAnsiEncoding /FirstChar 32 "
                f"/LastChar 126 /Widths [{widths}] /FontDescriptor << /Type /FontDescriptor "
                f"/FontName /{name} /Flags {flags} /FontBBox [0 -300 1000 900] /ItalicAngle 0 /Ascent 800 "
                f"/Descent -200 /CapHeight 700 /StemV 80 >> >>")
    content = b"BT /F1 18 Tf 20 120 Td (Hamburgefonstiv 0123) Tj 0 -30 Td (QUICK brown fox) Tj ET"
    objs = [b"<< /Type /Catalog /Pages 2 0 R >>", b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 160] /Resources << /Font << /F1 5 0 R >> >> "
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


def matched(trace):
    """The files fontconfig returned first (Poppler takes the first font
    of FcFontSort) or as the best match, in an FC_DEBUG=1 trace."""
    files, best = [], False
    for line in trace.splitlines():
        if line.startswith(("Best score", "First font")):
            best = True
        elif best:
            m = re.search(r'file: "([^"]+)"', line)
            if m:
                files.append(m.group(1))
                best = False
    return files


def render(tool, pdf, base, env):
    r = subprocess.run([tool, "-r", "72", "-gray", "-singlefile", pdf, base], capture_output=True, text=True,
                       env=dict(env, FC_DEBUG="1"), timeout=120)
    if r.returncode != 0:
        raise SystemExit(f"probe: {tool} failed on a probe: {r.stderr[-300:]}")
    return matched(r.stdout + r.stderr), hashlib.sha256(open(base + ".pgm", "rb").read()).hexdigest()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("e1dir")
    ap.add_argument("--json", required=True)
    a = ap.parse_args()
    e1 = os.path.abspath(a.e1dir)
    real = shutil.which("pdftoppm")
    wrapped = os.path.join(e1, "bin", "pdftoppm")
    if real and (os.path.realpath(real).startswith(e1 + os.sep) or os.environ.get("FONTCONFIG_FILE")):
        print("probe: E0 would not be the system's Poppler (PATH or FONTCONFIG_FILE points at E1)")
        return 2
    if not real or not os.access(wrapped, os.X_OK) or not shutil.which("fc-list"):
        print("probe: pdftoppm, fc-list or the E1 wrapper missing")
        return 2
    env = {k: v for k, v in os.environ.items() if k not in ("FONTCONFIG_FILE", "FONTCONFIG_PATH", "FC_DEBUG")}
    digest = lambda f: hashlib.sha256(open(f, "rb").read()).hexdigest() if os.path.isfile(f) else f
    # The environment E1's wrappers give fontconfig (E2's: its own HOME).
    e1env = dict(env, FONTCONFIG_FILE=os.path.join(e1, "fonts.conf"))
    if os.path.isdir(os.path.join(e1, "home")):
        e1env["HOME"] = os.path.join(e1, "home")
    fclist = lambda e: sorted(subprocess.run(["fc-list", "--format", "%{file}\\n"], capture_output=True, text=True,
                                             env=e).stdout.splitlines())
    e0_fonts = fclist(env)
    problems, rows = [], {}
    with tempfile.TemporaryDirectory() as tmp:
        for base in list(COURIER) + OTHERS:
            pdf = os.path.join(tmp, "p.pdf")
            open(pdf, "wb").write(probe_pdf(base))
            f0, img0 = render(real, pdf, os.path.join(tmp, "e0"), env)
            f1, img1 = render(wrapped, pdf, os.path.join(tmp, "e1"), env)
            rows[base] = {"e0": f0, "e1": f1, "same_pixels": img0 == img1}
            if any(f.startswith(e1 + "/") for f in f0):
                problems.append(f"{base}: E0 matched a file of E1")
            if base in COURIER:
                want = f"NimbusMonoPS-{COURIER[base]}.otf"
                if not f1 or any(os.path.basename(f) != want or not f.startswith(e1 + "/") for f in f1):
                    problems.append(f"{base}: E1 drew {f1}, not {want} of E1")
            elif [digest(f) for f in f0] != [digest(f) for f in f1] or img0 != img1:
                problems.append(f"{base}: E1 differs from E0 ({f0} -> {f1}, same pixels {img0 == img1})")
    if fclist(env) != e0_fonts:
        problems.append("the system font list changed")
    e1_fonts = fclist(e1env)
    e0_by = {digest(f): f for f in e0_fonts}
    added = sorted(f for f in e1_fonts if digest(f) not in e0_by)
    lacking = sorted(f for d, f in e0_by.items() if d not in {digest(x) for x in e1_fonts})
    if sorted(os.path.basename(f) for f in added) != sorted(f"NimbusMonoPS-{s}.otf" for s in ("Regular", "Bold", "Italic", "BoldItalic")) \
            or any(not f.startswith(e1 + "/") for f in added):
        problems.append(f"E1 adds other fonts than the four Nimbus Mono PS files: {added}")
    if lacking:
        problems.append(f"E1 lacks fonts E0 has: {lacking}")
    doc = {"probes": rows, "e0_font_count": len(e0_fonts), "e1_added": added, "problems": problems,
           "fonts_conf_sha256": hashlib.sha256(open(os.path.join(e1, "fonts.conf"), "rb").read()).hexdigest()}
    json.dump(doc, open(a.json, "w"), indent=1)
    for base, r in rows.items():
        print(f"{base:28} E0 {os.path.basename(r['e0'][0]) if r['e0'] else '-':32} "
              f"E1 {os.path.basename(r['e1'][0]) if r['e1'] else '-':32} {'same' if r['same_pixels'] else 'differs'}")
    for p in problems:
        print("PROBLEM:", p)
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
