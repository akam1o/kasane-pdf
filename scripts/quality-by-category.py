#!/usr/bin/env python3
"""Per-category quality tables over a kasane-compat run report.

    scripts/quality-by-category.py RUN.json [--files]

Prints one markdown table per dimension. Every category comes from a
measurement in the report (the file's /Producer family, the runner's
content profile, its features, size and page count); a file whose
property could not be measured (e.g. it did not parse) is `unknown` —
nothing is guessed from file names or sources. A file can sit in
several categories of one dimension (two font kinds, two scripts), so
the rows of a table can sum to more than the corpus.

Columns, for the `expect: pass` files of each category (n): strict
parse, strict render, of which exact (no reported approximation) and
approximate, recovered render (recovery mode only), strict text,
rewrite, incremental save, full PASS. `robust` is over the category's
`expect: reject` files: refused in strict mode without crash or
timeout. Aggregates only; `--files` is not implemented on purpose.
"""
import json
import sys
from collections import defaultdict


def strict(f, stage):
    return (f.get(stage) or {}).get("class") == "pass" and (stage == "parse" or not f.get("recovered"))


def refused(f):
    """An expect:reject file refused in strict mode, cleanly."""
    if f.get("verdict") in ("crash", "timeout"):
        return False
    return not (strict(f, "parse") and strict(f, "render"))


def size_bucket(b):
    for lim, name in ((100_000, "< 100 KB"), (1_000_000, "100 KB – 1 MB"), (10_000_000, "1 – 10 MB")):
        if b < lim:
            return name
    return "≥ 10 MB"


def pages_bucket(n):
    if n == 0:
        return "unknown"
    return "1" if n == 1 else "2–9" if n < 10 else "10–49" if n < 50 else "≥ 50"


def dims(f):
    p = f.get("profile") or {}
    parsed = (f.get("parse") or {}).get("class") == "pass" or f.get("recovered")
    unk = ["unknown"]
    kinds = p.get("font_kinds") or []
    feats = set(f.get("features") or [])
    tl = p.get("text_layer_page1")
    return {
        "generator (/Producer family)": [f.get("producer") or "unknown"],
        "script of extracted text": (p.get("scripts") or (["none extracted"] if parsed else unk)),
        "font kind": sorted(set(kinds)) or (["no fonts"] if parsed else unk),
        "font embedding": (
            sorted({"non-embedded" if k.endswith("-nonembedded") else "embedded" for k in kinds})
            or (["no fonts"] if parsed else unk)
        ),
        "text layer on page 1": [unk[0] if tl is None else ("present" if tl else "absent (image-only or vector-only)")],
        "encrypted": [("yes" if p.get("encrypted") else "no") if parsed else "unknown"],
        "forms / annotations / signatures": (
            sorted(feats & {"acroform", "annots", "signed"}) or (["none"] if parsed else unk)
        ),
        "transparency": [("yes" if "transparency" in feats else "no") if parsed else "unknown"],
        "shading": [("yes" if p.get("shading") else "no") if parsed else "unknown"],
        "image compression": (p.get("image_filters") or (["no images"] if parsed else unk)),
        "file size": [size_bucket(f.get("bytes") or 0)],
        "page count": [pages_bucket(f.get("total_pages") or 0)],
    }


def row(files):
    ps = [f for f in files if f.get("expect") == "pass"]
    rj = [f for f in files if f.get("expect") == "reject"]
    r = lambda pred: sum(1 for f in ps if pred(f))
    rend = lambda f: strict(f, "render")
    return {
        "n": len(ps),
        "parse": r(lambda f: strict(f, "parse")),
        "render": r(rend),
        "exact": r(lambda f: rend(f) and not f.get("render_approximate")),
        "approx": r(lambda f: rend(f) and f.get("render_approximate")),
        "recovered": r(lambda f: not rend(f) and (f.get("render_recovered") or {}).get("class") == "pass"),
        "text": r(lambda f: strict(f, "text")),
        "rewrite": r(lambda f: strict(f, "rewrite")),
        "incr": r(lambda f: (f.get("incremental") or {}).get("class") == "pass"),
        "full": r(lambda f: all(strict(f, s) for s in ("parse", "render", "text", "rewrite"))),
        "robust": f"{sum(1 for f in rj if refused(f))}/{len(rj)}" if rj else "–",
    }


COLS = ("n", "parse", "render", "exact", "approx", "recovered", "text", "rewrite", "incr", "full", "robust")


def main(argv):
    if len(argv) < 2:
        sys.stderr.write(__doc__)
        return 2
    run = json.load(open(argv[1]))
    files = run["files"]
    groups = defaultdict(lambda: defaultdict(list))
    for f in files:
        for dim, cats in dims(f).items():
            for c in cats:
                groups[dim][c].append(f)
    print(f"Population: {len(files)} files; columns over each category's expect:pass files (n), robust over its expect:reject files.\n")
    for dim, cats in groups.items():
        # Generators with fewer than three files are pooled: a row per
        # single file says nothing and points at the file.
        if dim.startswith("generator"):
            small = [c for c, fs in cats.items() if len(fs) < 3 and c != "unknown"]
            if small:
                pooled = [f for c in small for f in cats.pop(c)]
                cats[f"other ({len(small)} generators, < 3 files each)"] = pooled
        print(f"### {dim}\n")
        print("| category | " + " | ".join(COLS) + " |")
        print("|---|" + "---:|" * len(COLS))
        for c, fs in sorted(cats.items(), key=lambda kv: (-len(kv[1]), kv[0])):
            r = row(fs)
            print(f"| {c} | " + " | ".join(str(r[k]) for k in COLS) + " |")
        print()
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
