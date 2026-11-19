#!/usr/bin/env python3
"""Every page of a corpus rendered by Poppler in E0 and in E1: which pages
E1 changes, and which font files fontconfig gave Poppler for each file.

    scripts/courier-reference/reference-diff.py CORPUS_DIR E1DIR --json OUT [--dpi 96] [--jobs N]

The runner's reference (`pdftoppm -r DPI -gray`), all pages, decoded
pixels compared (PGM headers and file metadata play no part). The JSON
names files by their corpus id with page counts and the basenames of the
system font files matched — no page content; it is for the private
evaluation record, its counts for the public report. A file whose pages
E1 changes must be one whose E0 trace matched a Liberation Mono or
DejaVu Sans Mono face that E1 replaced for Courier — anything else is
listed as unexplained.
"""
import argparse, concurrent.futures, glob, json, os, re, subprocess, sys, tempfile

COURIER_E0 = ("LiberationMono-", "DejaVuSansMono")


def matched(trace):
    files, best = [], False
    for line in trace.splitlines():
        if line.startswith(("Best score", "First font")):
            best = True
        elif best:
            m = re.search(r'file: "([^"]+)"', line)
            if m:
                files.append(os.path.basename(m.group(1)))
                best = False
    return files


def pages(tool, pdf, out, dpi):
    os.makedirs(out, exist_ok=True)
    env = {k: v for k, v in os.environ.items() if k not in ("FONTCONFIG_FILE", "FONTCONFIG_PATH")}
    try:
        r = subprocess.run([tool, "-r", str(dpi), "-gray", pdf, os.path.join(out, "p")], capture_output=True,
                           text=True, errors="replace", env=dict(env, FC_DEBUG="1"), timeout=600)
    except subprocess.TimeoutExpired:
        return None, None, None
    imgs = {}
    for p in sorted(glob.glob(os.path.join(out, "p-*.pgm"))):
        data = open(p, "rb").read()
        parts = data.split(maxsplit=4)
        imgs[int(re.search(r"-(\d+)\.pgm$", p).group(1))] = (parts[1], parts[2], parts[4])
    # A damaged file can fail after some pages: what was drawn is compared,
    # the status recorded.
    return imgs, matched(r.stdout + r.stderr), r.returncode


def one(args):
    pdf, e1, dpi = args
    fid = os.path.splitext(os.path.basename(pdf))[0]
    with tempfile.TemporaryDirectory() as tmp:
        i0, f0, rc0 = pages("pdftoppm", pdf, os.path.join(tmp, "e0"), dpi)
        i1, f1, rc1 = pages(os.path.join(e1, "bin", "pdftoppm"), pdf, os.path.join(tmp, "e1"), dpi)
    if i0 is None or i1 is None:
        return fid, {"error": "timeout"}
    if rc0 != rc1:
        return fid, {"error": "status_differs", "status": [rc0, rc1]}
    diff = sorted(p for p in set(i0) | set(i1) if i0.get(p) != i1.get(p))
    return fid, {"pages": len(i0), "status": rc0, "pages_changed": len(diff), "changed_pages": diff,
                 "e0_fonts": sorted(set(f0)), "e1_fonts": sorted(set(f1))}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("corpus")
    ap.add_argument("e1")
    ap.add_argument("--json", required=True)
    ap.add_argument("--dpi", type=int, default=96)
    ap.add_argument("--jobs", type=int, default=4)
    a = ap.parse_args()
    pdfs = sorted(glob.glob(os.path.join(a.corpus, "*.pdf")))
    e1 = os.path.abspath(a.e1)
    res = {}
    with concurrent.futures.ThreadPoolExecutor(a.jobs) as ex:
        for fid, r in ex.map(one, [(p, e1, a.dpi) for p in pdfs]):
            res[fid] = r
    changed = {f: r for f, r in res.items() if r.get("pages_changed")}
    unexplained = sorted(f for f, r in changed.items()
                         if not any(x.startswith(COURIER_E0) for x in set(r["e0_fonts"]) - set(r["e1_fonts"]))
                         or not any(x.startswith("NimbusMonoPS-") for x in r["e1_fonts"])
                         or set(r["e1_fonts"]) - set(r["e0_fonts"]) - {x for x in r["e1_fonts"] if x.startswith("NimbusMonoPS-")})
    nimbus_unchanged = sorted(f for f, r in res.items() if "error" not in r and not r["pages_changed"]
                              and any(x.startswith("NimbusMonoPS-") for x in r["e1_fonts"]))
    summary = {"files": len(res), "errors": sorted(f for f, r in res.items() if "error" in r),
               "pages": sum(r.get("pages", 0) for r in res.values()),
               "files_changed": len(changed), "pages_changed": sum(r["pages_changed"] for r in changed.values()),
               "unexplained": unexplained, "nimbus_used_without_change": nimbus_unchanged,
               "files_using_nimbus_in_e1": sorted(f for f, r in res.items() if any(x.startswith("NimbusMonoPS-")
                                                                                     for x in r.get("e1_fonts", [])))}
    json.dump({"summary": summary, "files": res}, open(a.json, "w"), indent=1)
    print(json.dumps({k: (v if not isinstance(v, list) else len(v)) for k, v in summary.items()}))
    return 1 if unexplained or summary["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
