#!/usr/bin/env python3
"""The committable projection of a redaction corpus result: ids,
categories and numbers only.

    scripts/redact-results-projection.py IN.json OUT.json

IN is a result of scripts/eval-redact-corpus.py or
scripts/analyze-redact-refusals.py; OUT keeps an allowlist of fields —
the manifest id, the run's kind and category, its outcome, the refusal
reason category, and numeric counts — and drops everything else: the
rectangle (derived from the page's words), the producer, refusal
details and walk errors (they quote PDF names and tokens), examples.
This is what compat/redaction/ holds and what CI uploads; the raw
results stay where they were made.
"""
import json
import re
import sys

EVAL_TEXT = {"id", "kind", "category", "outcome"}
REFUSAL_TEXT = {"id", "kind"}
# A refusal reason is a category (kasane's feature name, or "other:
# <error kind>"), or the evaluator's own fixed sentence with a count.
# No "/", "[" or quotes: details and walk errors quote PDF names
# ("/Annots[0] (/Highlight) …"); categories never do.
REASON = re.compile(r"^(\d+ unchanged non-white pixel\(s\) inside the rect|[A-Za-z][A-Za-z0-9 :(),+-]{2,90})$")


def reason(v):
    if v is None:
        return None
    if not REASON.match(v):
        raise SystemExit(f"a reason that is not a category: {len(v)} characters")
    return v


def number(v):
    return v if isinstance(v, (int, float)) and not isinstance(v, bool) else None


def eval_run(r):
    out = {k: r[k] for k in EVAL_TEXT if k in r}
    if r.get("reason"):
        out["reason"] = reason(r["reason"])
    if "check_by_hand" in r:
        out["check_by_hand"] = bool(r["check_by_hand"])
    for k, v in r.items():
        if k not in out and k not in ("rect", "producer", "detail", "reason", "check_by_hand"):
            n = number(v)
            if n is not None:
                out[k] = n
    return out


def refusal_run(r):
    out = {k: r[k] for k in REFUSAL_TEXT if k in r}
    out["first_reason"] = reason(r.get("first_reason"))
    out["refused_by_walk"] = bool(r.get("refused_by_walk"))
    out["walk_categories"] = [reason(c) for c in r.get("walk_categories") or []]
    out["walk_findings"] = number(r.get("walk_findings"))
    return out


def summary(s):
    # Counts keyed by category, and plain counts.
    out = {}
    for k, v in s.items():
        if isinstance(v, dict):
            out[k] = {reason(c): number(n) for c, n in v.items()}
        else:
            out[k] = number(v)
    return out


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    d = json.load(open(sys.argv[1]))
    if "results" in d:
        out = {"results": [eval_run(r) for r in d["results"]],
               "skipped": [[s[0], reason(s[1])] for s in d.get("skipped", [])]}
    elif "runs" in d:
        out = {"summary": summary(d.get("summary") or {}), "runs": [refusal_run(r) for r in d["runs"]]}
    else:
        sys.exit("not a result of eval-redact-corpus.py or analyze-redact-refusals.py")
    with open(sys.argv[2], "w") as fh:
        json.dump(out, fh, indent=1, sort_keys=True)
        fh.write("\n")


if __name__ == "__main__":
    main()
