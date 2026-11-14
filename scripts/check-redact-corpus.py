#!/usr/bin/env python3
"""Compare a run of scripts/eval-redact-corpus.py with the committed
results in compat/redaction/ (nightly.yml, the release gate).

    scripts/check-redact-corpus.py <config> <new eval.json> [--baseline DIR] [--summary-md FILE]
                                   [--json FILE] [--strict-pixels]

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
  * a run's outcome is not one of ok, refused, unexpected;
  * a run `ok` on both sides lacks a measurement the committed run has
    (MEASURES; a witness that did not run is not a zero);
  * the set of runs differs;
  * with --strict-pixels (the local release gate, whose Poppler is the
    one the committed results were measured with): more pixels outside
    the rect changed than in the committed results.

Each change is classified on its own: a regression (above) fails; an
improvement (refused or unexpected before, `ok` now) and a changed
refusal reason are reported and never make a run pass — a higher `ok`
count with one regression still fails. --json writes the classification
(ids, kinds, closed-vocabulary codes and counts). Only ids, kinds and
counts are printed — never text, metadata or anything else from the
corpus files.
"""
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
LOST = ("outside_words_lost", "outside_chars_lost", "outside_chars_mupdf_lost", "outside_chars_mupdf_moved")
OUTCOMES = ("ok", "refused", "unexpected")
# Measurements of a successful run. A run `ok` on both sides must carry
# each one the committed run has, as a non-negative number: a witness
# that did not run (MuPDF failing, a render skipped) is not a zero.
MEASURES = LOST + ("outside_words", "outside_chars", "outside_chars_mupdf", "unchanged_ink_inside",
                   "outside_pixels_changed")


def measured(v):
    return isinstance(v, (int, float)) and not isinstance(v, bool) and v >= 0 and v == v


def runs(path):
    return {(r["id"], r["kind"]): r for r in json.load(open(path))["results"]}


def main():
    args = sys.argv[1:]
    summary = None
    baseline = os.path.join(ROOT, "compat", "redaction")
    out_json = None
    strict_pixels = "--strict-pixels" in args
    args = [a for a in args if a != "--strict-pixels"]
    for flag in ("--summary-md", "--baseline", "--json"):
        if flag in args:
            i = args.index(flag)
            if flag == "--summary-md":
                summary = args[i + 1]
            elif flag == "--json":
                out_json = args[i + 1]
            else:
                baseline = args[i + 1]
            del args[i:i + 2]
    if len(args) != 2:
        sys.exit(__doc__)
    config, new_path = args
    old = runs(os.path.join(baseline, f"eval-{config}.json"))
    new = runs(new_path)
    problems, notes = [], []
    regressions, improvements, reason_changes = [], [], []

    def regress(k, what, text):
        regressions.append({"id": k[0], "kind": k[1], "what": what})
        problems.append(f"{k[0]} {k[1]}: {text}")

    for k in sorted(set(old) - set(new)):
        regress(k, "run_missing", "missing from this run")
    for k in sorted(set(new) - set(old)):
        regress(k, "run_new", "not in the committed results")
    for k in sorted(set(old) & set(new)):
        o, n = old[k], new[k]
        label = f"{k[0]} {k[1]}"
        if n["outcome"] not in OUTCOMES:
            regress(k, "outcome_unknown", "an unknown outcome")
            continue
        if o["outcome"] == "ok" and n["outcome"] != "ok":
            regress(k, f"ok_to_{n['outcome']}", f"ok -> {n['outcome']}")
        if n["outcome"] == "unexpected" and not (n.get("check_by_hand") and n.get("unchanged_ink_not_from_images") == 0):
            regress(k, "unexpected", "unexpected (not the known zeroed-image signal)")
        if o["outcome"] != "ok" and n["outcome"] == "ok":
            improvements.append({"id": k[0], "kind": k[1], "what": f"{o['outcome']}_to_ok"})
            notes.append(f"{label}: {o['outcome']} -> ok")
        if o["outcome"] == n["outcome"] == "refused" and "reason" in o and o.get("reason") != n.get("reason"):
            reason_changes.append({"id": k[0], "kind": k[1], "what": "reason_changed"})
            notes.append(f"{label}: refusal reason changed")
        accepted_signal = n["outcome"] == "unexpected" and n.get("check_by_hand") and \
            n.get("unchanged_ink_not_from_images") == 0
        if o["outcome"] == n["outcome"] and (n["outcome"] == "ok" or accepted_signal):
            gone = [f for f in MEASURES if o.get(f) is not None and not measured(n.get(f))]
            if gone:
                regress(k, "measurement_missing", f"not measured: {', '.join(gone)}")
        for f in LOST:
            if (n.get(f) or 0) > (o.get(f) or 0):
                regress(k, f"more_{f}", f"{f} {o.get(f) or 0} -> {n.get(f)}")
        if strict_pixels and (n.get("outside_pixels_changed") or 0) > (o.get("outside_pixels_changed") or 0):
            regress(k, "more_outside_pixels_changed",
                    f"outside_pixels_changed {o.get('outside_pixels_changed') or 0} -> {n.get('outside_pixels_changed')}")
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
    if out_json:
        doc = {
            "config": config,
            "strict_pixels": strict_pixels,
            "counts": {side: {oc: count(d, oc) for oc in OUTCOMES} for side, d in (("committed", old), ("run", new))},
            "lost": {side: {f: total(d, f) for f in LOST} for side, d in (("committed", old), ("run", new))},
            "regressions": regressions,
            "improvements": improvements,
            "reason_changes": reason_changes,
            "status": "failed" if problems else "passed",
        }
        with open(out_json, "w") as fh:
            json.dump(doc, fh, indent=1, sort_keys=True)
            fh.write("\n")
    sys.exit(1 if problems else 0)


if __name__ == "__main__":
    main()
