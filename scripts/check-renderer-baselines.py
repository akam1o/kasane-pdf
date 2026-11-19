#!/usr/bin/env python3
"""The external-renderer baselines against the advisory inputs, without a
renderer.

    scripts/check-renderer-baselines.py --dump DIR [--history FILE] [--baselines DIR]
        [--platforms linux-x86_64,darwin-arm64]

DIR is an advisory dump (`KASANE_ADVISORY_DUMP=DIR cargo test --release
--test redact_advisories`). scripts/verify-renderers.py runs `redact` on
every input of it that has a `redact` command and needs each one in the
platform's baseline (`compat/renderers/baseline-<platform>.json`), with
the outcome compat/advisories/history.json expects of the candidate.
This checks, for every required platform, before anything is run:

- each file is JSON with no key twice, in the baselines' shape (its
  platform, schema 1, a reason, the three renderers' versions);
- every `adv:` case of the baseline is an input of the dump with a
  `redact` command, and every such input is in the baseline — an input
  added without its baseline entry (or the reverse) fails;
- every case's outcome is `ok` or `refused`, `refused` with no renderer
  result (nothing was written, nothing rendered) and `ok` with all
  three;
- every `adv:` outcome is the one the input must have: `refused` when its
  manifest says so, else the redaction outcome of history.json's
  candidate state (REMOVED, KEPT: `ok`; REFUSED: `refused`) — any other
  state, or none, fails;
- every input of the dump is in history.json and every input of
  history.json is in the dump.

`refused` is the outcome of kasane's `redact` (exit 1, a typed error,
no output) — not a renderer's verdict, and not whether the refusal's
message is quiet (scripts/check-advisory-regressions.py judges that).
What the renderers draw is verify-renderers.py's (Full Gate, release
verification). Exit 0 consistent, 1 not, 2 could not run.
"""
import argparse
import json
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
PLATFORMS = ("linux-x86_64", "darwin-arm64")
RENDERERS = ("poppler", "mupdf", "pdfjs")
OUTCOMES = ("ok", "refused")
HISTORY_OUTCOME = {"REMOVED": "ok", "KEPT": "ok", "REFUSED": "refused"}
TOP = {"cases", "dpi", "platform", "reason", "schema_version", "tools"}


class Unreadable(Exception):
    pass


def _pairs(pairs):
    out = {}
    for k, v in pairs:
        if k in out:
            raise ValueError(f"key {k!r} twice")
        out[k] = v
    return out


def load(path):
    try:
        with open(path) as fh:
            return json.load(fh, object_pairs_hook=_pairs)
    except (OSError, ValueError) as e:
        raise Unreadable(f"{os.path.basename(path)}: {e}")


def redact_inputs(dump):
    """{input file: (manifest expectation, has a redact command)}."""
    rows = {}
    names = sorted(f for f in os.listdir(dump) if f.startswith("manifest-") and f.endswith(".tsv"))
    for m in names:
        for line in open(os.path.join(dump, m)).read().splitlines():
            if not line.strip():
                continue
            parts = line.split("\t")
            if len(parts) != 4:
                raise Unreadable(f"{m}: a line without four fields")
            file, _ghsa, cmds, expect = parts
            if file in rows:
                raise Unreadable(f"{m}: {file} listed twice")
            rows[file] = (expect, any(c.startswith("redact ") for c in cmds.split("|")))
    return rows


def problems_of(dump, history, baselines, platforms):
    problems = []
    rows = redact_inputs(dump)
    if not rows:
        return ["no_inputs"]
    exp = history.get("expected") if isinstance(history, dict) else None
    if not isinstance(exp, dict) or not exp:
        return ["history_without_expectations"]
    for f in sorted(set(rows) - set(exp)):
        problems.append(f"input_not_in_history:{f}")
    for f in sorted(set(exp) - set(rows)):
        problems.append(f"history_input_not_dumped:{f}")
    want = {}
    for f, (expect, redact) in rows.items():
        if not redact:
            continue
        if expect == "refused":
            out = "refused"
        else:
            out = HISTORY_OUTCOME.get((exp.get(f) or {}).get("candidate"))
        if out is None:
            problems.append(f"no_outcome:{f}")
            continue
        want[f"adv:{f[:-4]}"] = out
    if not want:
        problems.append("no_redact_inputs")
    for platform in platforms:
        path = os.path.join(baselines, f"baseline-{platform}.json")
        try:
            b = load(path)
        except Unreadable as e:
            problems.append(f"{platform}:unreadable:{e}")
            continue
        if not isinstance(b, dict) or set(b) != TOP:
            problems.append(f"{platform}:shape")
            continue
        if b["platform"] != platform or b["schema_version"] != 1:
            problems.append(f"{platform}:platform_or_schema")
        if not isinstance(b["reason"], str) or not b["reason"].strip():
            problems.append(f"{platform}:no_reason")
        tools = b["tools"]
        if not isinstance(tools, dict) or set(tools) != set(RENDERERS) or not all(
                isinstance(v, str) and v for v in tools.values()):
            problems.append(f"{platform}:tools")
        cases = b["cases"]
        if not isinstance(cases, dict) or not cases:
            problems.append(f"{platform}:no_cases")
            continue
        for name, c in sorted(cases.items()):
            if not isinstance(c, dict) or c.get("outcome") not in OUTCOMES or not isinstance(c.get("renderers"), dict):
                problems.append(f"{platform}:malformed_case:{name}")
                continue
            if c["outcome"] == "refused" and c["renderers"]:
                problems.append(f"{platform}:refused_with_renders:{name}")
            if c["outcome"] == "ok" and set(c["renderers"]) != set(RENDERERS):
                problems.append(f"{platform}:ok_without_renders:{name}")
        have = {n for n in cases if n.startswith("adv:")}
        for n in sorted(set(want) - have):
            problems.append(f"{platform}:missing:{n}")
        for n in sorted(have - set(want)):
            problems.append(f"{platform}:not_an_input:{n}")
        for n in sorted(set(want) & have):
            got = cases[n].get("outcome") if isinstance(cases[n], dict) else None
            if got in OUTCOMES and got != want[n]:
                problems.append(f"{platform}:outcome:{n}:{got}!={want[n]}")
    return problems


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--dump", required=True)
    ap.add_argument("--history", default=os.path.join(ROOT, "compat", "advisories", "history.json"))
    ap.add_argument("--baselines", default=os.path.join(ROOT, "compat", "renderers"))
    ap.add_argument("--platforms", default=",".join(PLATFORMS))
    a = ap.parse_args(argv)
    platforms = [p for p in a.platforms.split(",") if p]
    if not platforms or any(p not in PLATFORMS for p in platforms):
        print(f"check-renderer-baselines: platforms must be among {', '.join(PLATFORMS)}")
        return 2
    try:
        if not os.path.isdir(a.dump):
            raise Unreadable(f"no dump at {a.dump}")
        problems = problems_of(a.dump, load(a.history), a.baselines, platforms)
    except Unreadable as e:
        print(f"check-renderer-baselines: cannot run: {e}")
        return 2
    for p in problems:
        print("PROBLEM:", p)
    if problems:
        print(f"check-renderer-baselines: {len(problems)} problem(s)")
        return 1
    print(f"check-renderer-baselines: consistent ({', '.join(platforms)})")
    return 0


if __name__ == "__main__":
    sys.exit(main())
