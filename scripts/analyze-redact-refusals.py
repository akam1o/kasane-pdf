#!/usr/bin/env python3
"""Why `redact` refuses Corpus runs — every blocker, not the first.

Reads a JSON result of `eval-redact-corpus.py` and, for each refused
run, re-runs the redaction walk with `redact --explain` (same page,
rect and options; nothing is written) to list every blocker and
uncertain item, groups them into categories, and estimates for each
category how many runs would get past the walk if it alone were
supported: the runs whose walk findings are all in that category and
whose refusal came from the walk. That is an upper bound — a later
stage (annotations, images being edited, the structure tree, the
output's verification) may still refuse.

Usage:
    scripts/analyze-redact-refusals.py <kasane-pdf> <eval.json> [--redact-arg ARG …] [--json out.json]
"""

import collections
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS = os.path.join(ROOT, "compat", "corpus")

# (category, pattern on a walk finding). First match wins.
CATEGORIES = [
    ("type3: CharProc draws an image or form (Do)", r"Type 3 glyph.*CharProc uses (Do|BI|inline)"),
    ("type3: CharProc uses another operator", r"Type 3 glyph.*CharProc uses"),
    ("type3: other", r"Type 3"),
    ("non-embedded font", r"non-embedded font"),
    ("vertical writing", r"vertical"),
    ("ActualText", r"ActualText"),
    ("text clipping mode (Tr 4-7)", r"text clipping mode|clip-mode text|Tr [4-7]"),
    ("strongly anisotropic text", r"anisotropic"),
    ("hairline stroke (0 w)", r"hairline"),
    ("stroke adjustment", r"stroke adjustment|/SA"),
    ("shading (sh)", r"\bsh\b|shading"),
    ("tiling pattern", r"tiling pattern"),
    ("soft mask / ExtGState", r"soft mask|SMask|ExtGState|/Font is not handled"),
    ("curve fill or dashed curve cut", r"curve|curved"),
    ("other path cut", r"cut|non-rectangular|overlapping|join"),
    ("inline image", r"inline image"),
    ("image", r"image"),
    ("form XObject", r"form|XObject"),
    ("optional content", r"optional content|/OC"),
]


def category(what):
    for name, pat in CATEGORIES:
        if re.search(pat, what, re.I):
            return name
    return "other: " + re.sub(r"0x[0-9A-F]+|\d+(\.\d+)?", "#", what)[:80]


def main():
    args = sys.argv[1:]
    out_json = None
    extra = []
    rest = []
    i = 0
    while i < len(args):
        if args[i] == "--json":
            out_json = args[i + 1]
            i += 2
        elif args[i] == "--redact-arg":
            extra.append(args[i + 1])
            i += 2
        else:
            rest.append(args[i])
            i += 1
    binary, eval_json = rest
    walk_args = [a for a in extra if a == "--embed-substitute-fonts"]
    results = json.load(open(eval_json))["results"]
    runs = []
    for r in results:
        if r.get("outcome") != "refused":
            continue
        pdf = os.path.join(CORPUS, r["id"] + ".pdf")
        if not os.path.isfile(pdf):
            pdf = os.path.join(CORPUS, r["id"])
        rect = [str(v) for v in r["rect"]]
        p = subprocess.run([binary, "redact", pdf, "--page", "1", "--rect", *rect, *walk_args, "--explain"],
                           capture_output=True, text=True)
        findings = []
        if p.returncode == 0:
            for line in p.stdout.splitlines():
                f = json.loads(line)
                findings.append({"kind": f["kind"], "what": f["what"], "category": category(f["what"])})
            walk_error = None
        else:
            walk_error = (p.stderr or p.stdout).strip().splitlines()[-1][:200]
        cats = sorted({f["category"] for f in findings})
        from_walk = r["reason"].startswith("region removal on a page with")
        runs.append({
            "id": r["id"], "kind": r["kind"], "rect": r["rect"],
            "first_reason": r["reason"], "first_detail": r.get("detail", "")[:200],
            "refused_by_walk": from_walk, "walk_error": walk_error,
            "walk_categories": cats,
            "walk_findings": len(findings),
            "examples": {c: next(f["what"][:160] for f in findings if f["category"] == c) for c in cats},
        })
    by_reason = collections.Counter(r["first_reason"] for r in runs)
    walk_runs = [r for r in runs if r["refused_by_walk"]]
    cat_runs = collections.Counter(c for r in walk_runs for c in r["walk_categories"])
    only = collections.Counter(r["walk_categories"][0] for r in walk_runs if len(r["walk_categories"]) == 1)
    multi = sum(1 for r in walk_runs if len(r["walk_categories"]) > 1)
    files = collections.defaultdict(set)
    for r in walk_runs:
        for c in r["walk_categories"]:
            files[c].add(r["id"])
    summary = {
        "refused_runs": len(runs),
        "first_reasons": dict(by_reason.most_common()),
        "walk_refusals": len(walk_runs),
        "walk_refusals_with_several_categories": multi,
        "walk_category_runs": dict(cat_runs.most_common()),
        "walk_category_files": {c: len(v) for c, v in sorted(files.items(), key=lambda kv: -len(kv[1]))},
        "would_pass_the_walk_if_only_this_were_supported": dict(only.most_common()),
    }
    print(f"refused runs: {len(runs)}; refused by the walk: {len(walk_runs)} "
          f"({multi} with several categories)")
    print("walk categories (runs / files / runs where it is the only one):")
    for c, n in cat_runs.most_common():
        print(f"  {n:4} {len(files[c]):3} {only.get(c, 0):4}  {c}")
    print("first refusal reasons:")
    for k, n in by_reason.most_common():
        print(f"  {n:4}  {k}")
    if out_json:
        json.dump({"summary": summary, "runs": runs}, open(out_json, "w"), indent=1, ensure_ascii=False)


if __name__ == "__main__":
    main()
