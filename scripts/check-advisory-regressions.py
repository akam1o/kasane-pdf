#!/usr/bin/env python3
"""Run the advisory regression inputs against a kasane-pdf CLI.

    KASANE_ADVISORY_DUMP=/tmp/adv cargo test --test redact_advisories
    scripts/check-advisory-regressions.py <kasane-pdf> /tmp/adv [--expect-safe]

Each input of `tests/redact_advisories.rs` is run with the arguments in
`manifest-*.tsv` (alternative commands separated by `|`: the first the CLI's usage
lists is run) and judged with tools that share no code with kasane:

- `refused`: SAFE when the CLI refuses with a typed error, else LEAK (or
  CRASH: no output without a typed error);
- `safe-text:T`: SAFE when refused or when `pdftotext` does not extract T;
- `safe-bytes:T` (`hex:..` for bytes): SAFE when refused or when T is not
  in the output after `qpdf --qdf --object-streams=disable
  --decode-level=all`;
- `fast`: SAFE when the command ends within 15 s, else SLOW.

A CLI without the subcommand (older versions) gives N/A. With
`--expect-safe` the exit status is 1 unless every case is SAFE.
"""
import os
import subprocess
import sys
import tempfile
import time


def main():
    args = [a for a in sys.argv[1:] if not a.startswith("--")]
    expect_safe = "--expect-safe" in sys.argv
    if len(args) != 2:
        print(__doc__)
        return 2
    kasane, d = args
    rows = []
    for f in sorted(os.listdir(d)):
        if f.startswith("manifest-") and f.endswith(".tsv"):
            rows += [l.rstrip("\n").split("\t") for l in open(os.path.join(d, f)) if l.strip()]
    bad = 0
    usage = subprocess.run([kasane], capture_output=True, text=True)
    usage = usage.stdout + usage.stderr
    with tempfile.TemporaryDirectory() as tmp:
        for name, ghsa, cli, expect in rows:
            src = os.path.join(d, name)
            out = os.path.join(tmp, "out.pdf")
            if os.path.exists(out):
                os.remove(out)
            available = [a for a in cli.split("|") if f" {a.split()[0]} " in usage]
            if not available:
                print(f"{ghsa:22} {name:32} {'N/A':16} {0:6.2f}s")
                # The candidate must run every case; N/A is for older CLIs.
                bad += expect_safe
                continue
            for alt in available[:1]:
                parts = alt.split()
                cmd = [kasane, parts[0], src] + parts[1:]
                product = out if parts[0] != "render" else os.path.join(tmp, "out.png")
                if os.path.exists(product):
                    os.remove(product)
                cmd += ["--output", product]
                t = time.time()
                try:
                    p = subprocess.run(cmd, capture_output=True, text=True, timeout=60)
                    timed_out = False
                except subprocess.TimeoutExpired:
                    p, timed_out = None, True
                elapsed = time.time() - t
            # The CLI's result first: a completed success with its output,
            # or a typed refusal (exit status 1, an "error [" line, no
            # output). A signal, another status, or a success without
            # output is a crash — whatever the expectation.
            if timed_out:
                result = "slow"
            elif p.returncode == 0 and os.path.isfile(product):
                result = "done"
            elif p.returncode == 1 and "error [" in p.stderr and not os.path.exists(product):
                result = "refused"
            else:
                result = "crash"
            if result == "slow":
                verdict = "SLOW"
            elif result == "crash":
                verdict = f"CRASH (exit {p.returncode})"
            elif expect == "fast":
                verdict = "SLOW" if elapsed > 15 else "SAFE"
            elif result == "refused":
                verdict = "SAFE (refused)"
            elif expect == "refused":
                verdict = "LEAK (accepted)"
            elif expect.startswith("safe-text:"):
                # The witness must have run: an extraction that failed is
                # not "the secret is absent".
                w = subprocess.run(["pdftotext", out, "-"], capture_output=True, text=True)
                if w.returncode != 0:
                    verdict = "ERROR (witness)"
                else:
                    verdict = "LEAK" if expect[10:] in w.stdout.replace("\n", "") else "SAFE (removed)"
            elif expect.startswith("safe-bytes:"):
                want = expect[11:]
                want = bytes.fromhex(want[4:]) if want.startswith("hex:") else want.encode()
                w = subprocess.run(["qpdf", "--qdf", "--object-streams=disable", "--decode-level=all", out, "-"],
                                   capture_output=True)
                if w.returncode not in (0, 3) or not w.stdout.startswith(b"%PDF-"):
                    verdict = "ERROR (witness)"
                else:
                    verdict = "LEAK" if want in w.stdout else "SAFE (removed)"
            else:
                verdict = f"? {expect}"
            if not verdict.startswith("SAFE"):
                bad += 1
            print(f"{ghsa:22} {name:32} {verdict:16} {elapsed:6.2f}s")
    return 1 if expect_safe and bad else 0


if __name__ == "__main__":
    sys.exit(main())
