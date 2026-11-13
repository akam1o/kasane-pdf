#!/usr/bin/env python3
"""Compare a run of scripts/eval-redact-corpus.py with the committed
results in compat/redaction/ (nightly.yml, the release gate).

    scripts/check-redact-corpus.py <config> <new eval.json> [--baseline DIR] [--summary-md FILE]

<config> is one of default, optin, optin-javascript, optin-strict; the
results compared with are DIR/eval-<config>.json (default
compat/redaction/, measured with the maintainer's Poppler and MuPDF;
CI compares with compat/redaction/ci/ when it exists, measured with the
runner's — the checks render and extract text with those tools, and
their versions shift word and pixel counts). Fails when:

  * a run the committed results have as `ok` is not `ok` now;
  * a run is `unexpected` now without being the known false signal
    (`check_by_hand` with `unchanged_ink_not_from_images` 0 — zeroed
    image samples that were already dark);
  * words, characters (Poppler or MuPDF) or characters' positions kept
    outside the rects are lost more than in the committed results;
  * the set of runs differs.

A run refused before and `ok` now is reported (an improvement: commit
the new results). Only ids, kinds and counts are printed — never text,
metadata or anything else from the corpus files.
"""
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LOST = ("outside_words_lost", "outside_chars_lost", "outside_chars_mupdf_lost", "outside_chars_mupdf_moved")


def runs(path):
    return {(r["id"], r["kind"]): r for r in json.load(open(path))["results"]}


def main():
    args = sys.argv[1:]
    summary = None
    baseline = os.path.join(ROOT, "compat", "redaction")
    for flag in ("--summary-md", "--baseline"):
        if flag in args:
            i = args.index(flag)
            if flag == "--summary-md":
                summary = args[i + 1]
            else:
                baseline = args[i + 1]
            del args[i:i + 2]
    if len(args) != 2:
        sys.exit(__doc__)
    config, new_path = args
    old = runs(os.path.join(baseline, f"eval-{config}.json"))
    new = runs(new_path)
    problems, notes = [], []
    if set(old) != set(new):
        problems.append(f"run set differs: {len(set(old) - set(new))} missing, {len(set(new) - set(old))} new")
    for k in sorted(set(old) & set(new)):
        o, n = old[k], new[k]
        label = f"{k[0]} {k[1]}"
        if o["outcome"] == "ok" and n["outcome"] != "ok":
            problems.append(f"{label}: ok -> {n['outcome']}")
        if n["outcome"] == "unexpected" and not (n.get("check_by_hand") and n.get("unchanged_ink_not_from_images") == 0):
            problems.append(f"{label}: unexpected (not the known zeroed-image signal)")
        if o["outcome"] != "ok" and n["outcome"] == "ok":
            notes.append(f"{label}: {o['outcome']} -> ok")
        for f in LOST:
            if (n.get(f) or 0) > (o.get(f) or 0):
                problems.append(f"{label}: {f} {o.get(f) or 0} -> {n.get(f)}")
    count = lambda d, oc: sum(r["outcome"] == oc for r in d.values())
    total = lambda d, f: sum(r.get(f) or 0 for r in d.values())
    lines = [
        f"### redaction corpus — {config} (vs {os.path.relpath(baseline, ROOT)})",
        "",
        "| | committed | this run |",
        "| --- | ---: | ---: |",
        f"| ok | {count(old, 'ok')} | {count(new, 'ok')} |",
        f"| refused | {count(old, 'refused')} | {count(new, 'refused')} |",
        f"| unexpected | {count(old, 'unexpected')} | {count(new, 'unexpected')} |",
    ] + [f"| {f} | {total(old, f)} | {total(new, f)} |" for f in LOST] + [""]
    lines += [f"- problem: {p}" for p in problems] + [f"- improved: {n}" for n in notes]
    text = "\n".join(lines) + "\n"
    print(text)
    if summary:
        with open(summary, "a") as fh:
            fh.write(text)
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
