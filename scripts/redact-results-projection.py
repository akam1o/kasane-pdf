#!/usr/bin/env python3
"""The committable projection of a redaction corpus result: ids,
categories from a closed vocabulary, and named counts only.

    scripts/redact-results-projection.py IN.json OUT.json

IN is a result of scripts/eval-redact-corpus.py or
scripts/analyze-redact-refusals.py. OUT keeps:

  * per run: the manifest id, the run's kind and category, its outcome,
    whether it was checked mechanically (`check_by_hand`), the counts
    named in METRICS, and for a refused run its reason in the closed
    vocabulary below;
  * for refusals: the first reason and the walk categories, each
    replaced by "other" unless it is in the closed vocabulary —
    kasane's refusal features (`feature: "…"` in src/), "other: <error
    kind>" (src/error.rs), a feature "… with blockers: <blocker>" whose
    blocker is one kasane names (`what: "…"`), and the category names of
    scripts/analyze-redact-refusals.py.

Everything else is dropped: the rectangle (derived from the page's
words), the producer, the evaluator's reasons for unexpected runs,
refusal details and walk errors (they quote PDF names and tokens),
examples, and any field not named here. compat/redaction/ holds these
projections and CI uploads only them; the raw results stay where they
were made.
"""
import json
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
METRICS = (
    "seconds", "elsewhere", "codes_removed", "images_edited", "forms_edited", "paths_removed",
    "paths_cut_at_a_rect", "shadings_removed", "images_unchanged", "unchanged_ink_inside",
    "unchanged_ink_not_from_images", "outside_words", "outside_words_lost", "outside_chars",
    "outside_chars_lost", "outside_chars_mupdf", "outside_chars_mupdf_lost", "outside_chars_mupdf_moved",
    "words_regrouped", "outside_pixels_changed",
)
OUTCOMES = {"ok", "refused", "unexpected", "incomplete"}
# Text measurements: which, their states and reasons (closed; mirrors
# scripts/eval-redact-corpus.py — TEXT_STATES, TEXT_REASONS).
MEASUREMENTS = ("poppler_in", "poppler_out", "mupdf_in", "mupdf_out")
TEXT_STATES = ("success", "empty", "error", "timeout", "incomplete", "incomparable")
TEXT_REASONS = ("not_installed", "exit_status", "crashed", "timeout", "no_output", "encoding", "corrupt_output",
                "no_page", "page_count", "page_size", "char_fields", "empty_while_poppler_has_text",
                "empty_while_mupdf_has_text", "not_measured")


def vocabulary():
    src = " ".join(p.read_text() for p in sorted((ROOT / "src").glob("*.rs")))
    features = set(re.findall(r'feature: "([^"]+)"', src))
    err = (ROOT / "src/error.rs").read_text()
    kinds = set(re.findall(r'ErrorKind::\w+ => "([^"]+)"', err))
    blockers = {re.split(r"[:—(]", w, maxsplit=1)[0].strip() for w in re.findall(r'what: "([^"]+)"', src)}
    analyze = (ROOT / "scripts/analyze-redact-refusals.py").read_text()
    categories = set(re.findall(r'^\s+\("([^"]+)", r"', analyze, re.M))
    vocab = features | {"other: " + k for k in kinds} | categories
    vocab |= {f"{f}: {b}" for f in features if f.endswith("with blockers") for b in blockers if b}
    return vocab


VOCAB = vocabulary()


def term(v):
    return v if v in VOCAB else "other"


def number(v):
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def eval_run(r):
    out = {k: str(r[k]) for k in ("id", "kind", "category") if k in r}
    out["outcome"] = r["outcome"] if r.get("outcome") in OUTCOMES else "unexpected"
    if "check_by_hand" in r:
        out["check_by_hand"] = bool(r["check_by_hand"])
    if out["outcome"] == "refused":
        out["reason"] = term(r.get("reason"))
    tm = r.get("text_measures")
    if isinstance(tm, dict):
        out["text_measures"] = {k: (tm.get(k) if tm.get(k) in TEXT_STATES else "incomplete") for k in MEASUREMENTS
                                if k in tm}
        why = r.get("text_measure_reasons") or {}
        reasons = {k: (why[k] if why.get(k) in TEXT_REASONS else "not_measured") for k in MEASUREMENTS if k in why}
        if reasons:
            out["text_measure_reasons"] = reasons
    for k in METRICS:
        n = number(r.get(k))
        if n is not None:
            out[k] = n
    return out


def refusal_run(r):
    return {
        "id": str(r.get("id")),
        "kind": str(r.get("kind")),
        "first_reason": term(r.get("first_reason")),
        "refused_by_walk": bool(r.get("refused_by_walk")),
        "walk_categories": [term(c) for c in r.get("walk_categories") or []],
        "walk_findings": number(r.get("walk_findings")),
    }


def summary(s):
    out = {}
    for k, v in s.items():
        if isinstance(v, dict):
            m = {}
            for c, n in v.items():
                m[term(c)] = m.get(term(c), 0) + (number(n) or 0)
            out[k] = m
        else:
            out[k] = number(v)
    return out


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    d = json.load(open(sys.argv[1]))
    if "results" in d:
        out = {"results": [eval_run(r) for r in d["results"]],
               "skipped": [str(s[0]) for s in d.get("skipped", [])]}
    elif "runs" in d:
        out = {"summary": summary(d.get("summary") or {}), "runs": [refusal_run(r) for r in d["runs"]]}
    else:
        sys.exit("not a result of eval-redact-corpus.py or analyze-redact-refusals.py")
    with open(sys.argv[2], "w") as fh:
        json.dump(out, fh, indent=1, sort_keys=True)
        fh.write("\n")


if __name__ == "__main__":
    main()
