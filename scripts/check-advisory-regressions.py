#!/usr/bin/env python3
"""Run the advisory regression inputs against a kasane-pdf CLI.

    KASANE_ADVISORY_DUMP=/tmp/adv cargo test --test redact_advisories
    scripts/check-advisory-regressions.py <kasane-pdf> /tmp/adv [--expect-safe]

Each input of `tests/redact_advisories.rs` is run with the arguments in
`manifest-*.tsv` (alternative commands separated by `|`: the first the CLI's usage
lists is run) and judged with tools that share no code with kasane.
`judge_case` gives one state per case (scripts/advisory-history.py uses
it for every published version):

- the CLI's result first: completed (exit 0 with its output file),
  refused (exit 1, an "error [" line, no output), crashed (a signal,
  another status, a success without output), timed out, or N/A (the CLI
  has no such command);
- then the expectation, on a completed result:
  `safe-text:T` — LEAK when `pdftotext` extracts T, else REMOVED;
  `safe-bytes:T` (`hex:..` for bytes) — LEAK when T is in the output
  after `qpdf --qdf --object-streams=disable --decode-level=all`, else
  REMOVED; `kept-bytes:T` — KEPT when T is still there (data that must
  stay, a shared action), else LOST; `refused` — LEAK (accepted what it
  must refuse); `fast` — FAST within 15 s, else SLOW.
  A witness that fails (`pdftotext` not exiting 0, `qpdf` not exiting 0
  or 3 or not writing a PDF) is ERROR, never "absent".

States: LEAK, REMOVED, REFUSED, KEPT, LOST, FAST, SLOW, TIMEOUT, CRASH,
ERROR, NA. REFUSED is a state of its own: whether a refusal is the
expected safe behaviour of a case is the caller's decision. The printed
verdicts keep their old form — `SAFE (removed)`, `SAFE (refused)`,
`SAFE (kept)`, `SAFE` (fast) — and with `--expect-safe` the exit status
is 1 unless every case is one of them (N/A included as a failure: the
candidate must run every case).
"""
import os
import subprocess
import sys
import tempfile
import time

STATES = ("LEAK", "REMOVED", "REFUSED", "KEPT", "LOST", "FAST", "SLOW", "TIMEOUT", "CRASH", "ERROR", "NA")
SAFE_STATES = ("REMOVED", "REFUSED", "KEPT", "FAST")


def read_manifests(d):
    rows = []
    for f in sorted(os.listdir(d)):
        if f.startswith("manifest-") and f.endswith(".tsv"):
            for line in open(os.path.join(d, f)):
                if line.strip():
                    parts = line.rstrip("\n").split("\t")
                    if len(parts) != 4:
                        raise SystemExit(f"{f}: a line without four fields")
                    rows.append(parts)
    return rows


def usage_of(kasane):
    u = subprocess.run([kasane], capture_output=True, text=True)
    return u.stdout + u.stderr


def needle(expect):
    want = expect.split(":", 1)[1]
    return bytes.fromhex(want[4:]) if want.startswith("hex:") else want.encode()


def judge_case(kasane, src, cli, expect, tmp, usage, timeout=60):
    """{state, elapsed, exit, output (path or None), command (list)}."""
    out = os.path.join(tmp, "out.pdf")
    available = [a for a in cli.split("|") if f" {a.split()[0]} " in usage]
    if not available:
        return {"state": "NA", "elapsed": 0.0, "exit": None, "output": None, "command": None}
    parts = available[0].split()
    product = out if parts[0] != "render" else os.path.join(tmp, "out.png")
    for f in (out, product):
        if os.path.exists(f):
            os.remove(f)
    cmd = [kasane, parts[0], src] + parts[1:] + ["--output", product]
    t = time.time()
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=timeout)
    except subprocess.TimeoutExpired:
        return {"state": "TIMEOUT", "elapsed": time.time() - t, "exit": None, "output": None, "command": parts[0]}
    elapsed = time.time() - t
    res = {"elapsed": elapsed, "exit": p.returncode, "output": None, "command": parts[0]}
    if p.returncode == 0 and os.path.isfile(product):
        result = "done"
    elif p.returncode == 1 and "error [" in p.stderr and not os.path.exists(product):
        result = "refused"
    else:
        return dict(res, state="CRASH")
    if expect == "fast":
        return dict(res, state="FAST" if elapsed <= 15 else "SLOW")
    if result == "refused":
        return dict(res, state="REFUSED")
    res["output"] = out
    if expect == "refused":
        return dict(res, state="LEAK")
    if expect.startswith("safe-text:"):
        w = subprocess.run(["pdftotext", out, "-"], capture_output=True, text=True)
        if w.returncode != 0:
            return dict(res, state="ERROR")
        return dict(res, state="LEAK" if expect[10:] in w.stdout.replace("\n", "") else "REMOVED")
    if expect.startswith(("safe-bytes:", "kept-bytes:")):
        w = subprocess.run(["qpdf", "--qdf", "--object-streams=disable", "--decode-level=all", out, "-"],
                           capture_output=True)
        if w.returncode not in (0, 3) or not w.stdout.startswith(b"%PDF-"):
            return dict(res, state="ERROR")
        there = needle(expect) in w.stdout
        if expect.startswith("kept-bytes:"):
            return dict(res, state="KEPT" if there else "LOST")
        return dict(res, state="LEAK" if there else "REMOVED")
    return dict(res, state="ERROR")


PRINTED = {"REMOVED": "SAFE (removed)", "REFUSED": "SAFE (refused)", "KEPT": "SAFE (kept)", "FAST": "SAFE",
           "TIMEOUT": "SLOW (timeout)", "NA": "N/A", "ERROR": "ERROR (witness)"}


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    expect_safe = "--expect-safe" in sys.argv
    if len(args) != 2:
        print(__doc__)
        return 2
    kasane, d = args
    rows = read_manifests(d)
    bad = 0
    usage = usage_of(kasane)
    with tempfile.TemporaryDirectory() as tmp:
        for name, ghsa, cli, expect in rows:
            r = judge_case(kasane, os.path.join(d, name), cli, expect, tmp, usage)
            state = r["state"]
            if state == "LEAK" and expect == "refused":
                verdict = "LEAK (accepted)"
            elif state == "CRASH":
                verdict = f"CRASH (exit {r['exit']})"
            else:
                verdict = PRINTED.get(state, state)
            if state not in SAFE_STATES:
                bad += 1
            print(f"{ghsa:22} {name:32} {verdict:16} {r['elapsed']:6.2f}s")
    return 1 if expect_safe and bad else 0


if __name__ == "__main__":
    sys.exit(main())
