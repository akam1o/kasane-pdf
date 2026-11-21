#!/usr/bin/env python3
"""The units of a sharded reference-diff run (reference-diff.py --shard
--chunk), joined — or the reason they cannot be.

    scripts/courier-reference/reference-diff-merge.py ARTIFACTS_DIR --shards N --chunks M --json OUT
        [--summary-md FILE]

ARTIFACTS_DIR holds one directory per uploaded unit,
`reference-diff-s<I>-c<J>-a<attempt>/` with `unit.json` (the unit's
result, written whole at its end), `progress.jsonl` and `monitor.jsonl`
(diagnostics). For each of the N × M units the newest attempt whose
unit.json is complete is taken; a unit is never assembled from two
attempts. The result is the full corpus only when:
- every unit is there and complete;
- every unit names the same corpus (file count and digest), harness
  digest, DPI, commit and run, and the same E0 and E1 fingerprints;
- the units' files are disjoint, each the one its index selects, and
  together the whole corpus.
Otherwise (exit 2) nothing is reported as compared: the missing and
partial units are listed with what their progress and monitor files
saved (the files done, the last file, the last resource sample) — the
diagnostics of a lost attempt, as far as they were uploaded before it
was lost. Exit 1: the merged result has unexplained changes or errors;
0: the full corpus, compared, everything explained.
"""
import argparse, importlib.util, json, os, re, sys

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("reference_diff", os.path.join(HERE, "reference-diff.py"))
rd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(rd)

NAME = re.compile(r"reference-diff-s(\d+)-c(\d+)-a(\d+)$")
SAME = ("corpus", "harness_sha256", "dpi", "references")


def jsonl(path):
    out = []
    try:
        with open(path, errors="replace") as fh:
            for line in fh:
                try:
                    out.append(json.loads(line))
                except ValueError:
                    out.append({"event": "unreadable"})  # a line cut by a lost runner
    except OSError:
        pass
    return out


def load(root):
    """{(shard, chunk): {attempt: {'unit': dict | None, 'progress': [...], 'monitor': [...]}}}."""
    units = {}
    for d in sorted(os.listdir(root)):
        m = NAME.match(d)
        if not m:
            continue
        s, c, a = (int(x) for x in m.groups())
        path = os.path.join(root, d)
        try:
            with open(os.path.join(path, "unit.json")) as fh:
                unit = json.load(fh)
        except (OSError, ValueError):
            unit = None
        units.setdefault((s, c), {})[a] = {"unit": unit, "progress": jsonl(os.path.join(path, "progress.jsonl")),
                                           "monitor": jsonl(os.path.join(path, "monitor.jsonl"))}
    return units


def lost(entry):
    """What an incomplete attempt saved."""
    files = [p for p in entry["progress"] if p.get("event") == "file"]
    mon = [m for m in entry["monitor"] if "mem_available_kb" in m]
    return {"files_done": len(files), "last_file": files[-1]["id"] if files else None,
            "ended": any(p.get("event") == "end" for p in entry["progress"]),
            "last_sample": mon[-1] if mon else None, "samples": len(mon)}


def merge(units, shards, chunks):
    problems, provenance, partial, taken = [], {}, [], {}
    for s in range(1, shards + 1):
        for c in range(1, chunks + 1):
            attempts = units.get((s, c)) or {}
            ok = [a for a, e in attempts.items() if e["unit"] and e["unit"].get("complete")
                  and e["unit"].get("unit", {}).get("shard") == [s, shards]
                  and e["unit"].get("unit", {}).get("chunk") == [c, chunks]]
            for a, e in sorted(attempts.items()):
                if a not in ok:
                    partial.append(dict(unit=f"{s}/{shards}.{c}/{chunks}", attempt=a, **lost(e)))
            if not ok:
                problems.append(f"unit shard {s}/{shards} chunk {c}/{chunks}: no complete attempt "
                                f"({len(attempts)} uploaded)")
                continue
            a = max(ok)
            taken[(s, c)] = attempts[a]["unit"]
            provenance[f"{s}.{c}"] = a
    if problems:
        return None, problems, provenance, partial
    first = next(iter(taken.values()))
    for (s, c), u in taken.items():
        for k in SAME:
            if u.get(k) != first.get(k):
                problems.append(f"unit {s}.{c}: {k} differs from unit 1.1")
        for k in ("commit", "run_id"):
            if (u.get("run") or {}).get(k) != (first.get("run") or {}).get(k):
                problems.append(f"unit {s}.{c}: run {k} differs from unit 1.1")
        if sorted(u.get("files") or {}) != sorted(u["unit"]["files"]):
            problems.append(f"unit {s}.{c}: its results are not the files it took")
    n = first["corpus"]["files"]
    names = sorted(f for u in taken.values() for f in u["unit"]["files"])
    if len(names) != len(set(names)):
        problems.append("a file is in two units")
    if len(set(names)) != n:
        problems.append(f"the units cover {len(set(names))} files, the corpus has {n}")
    # Each unit's files are the ones its index selects in the sorted corpus.
    # (sorted as reference-diff.py sorts the corpus: by file name, `.pdf`
    # included — `a-b.pdf` before `a.pdf`)
    order = sorted(names, key=lambda x: x + ".pdf")
    want = {(s, c): [os.path.splitext(os.path.basename(p))[0]
                     for p in rd.select([x + ".pdf" for x in order], (s, shards), (c, chunks))]
            for (s, c) in taken}
    for k, u in taken.items():
        if u["unit"]["files"] != want[k]:
            problems.append(f"unit {k[0]}.{k[1]}: not the files its index selects")
    if problems:
        return None, problems, provenance, partial
    res = {}
    for u in taken.values():
        res.update(u["files"])
    return res, [], provenance, partial


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("artifacts")
    ap.add_argument("--shards", type=int, required=True)
    ap.add_argument("--chunks", type=int, required=True)
    ap.add_argument("--json", required=True)
    ap.add_argument("--summary-md")
    a = ap.parse_args()
    units = load(a.artifacts)
    res, problems, provenance, partial = merge(units, a.shards, a.chunks)
    doc = {"complete": res is not None, "problems": problems, "attempts": provenance, "incomplete_attempts": partial}
    if res is not None:
        first = next(e["unit"] for att in units.values() for e in att.values() if e["unit"])
        doc.update({k: first.get(k) for k in SAME}, run=first.get("run"), summary=rd.summarize(res), files=res)
    with open(a.json, "w") as fh:
        json.dump(doc, fh, indent=1)
    lines = [f"### reference-diff: {'complete' if res is not None else '**incomplete**'}", ""]
    if res is not None:
        s = doc["summary"]
        lines.append(f"- {s['files']} files: {len(s['coverage_complete'])} compared in full, "
                     f"{len(s['coverage_partial'])} partly, {len(s['coverage_none'])} not at all; "
                     f"{s['pages_changed']} pages of {s['files_changed']} files changed, "
                     f"{len(s['unexplained'])} unexplained, {len(s['errors'])} errors")
    lines.append(f"- units taken (unit: attempt): {provenance}")
    for p in problems:
        lines.append(f"- **{p}**")
    for p in partial:
        lines.append(f"- incomplete attempt {p['attempt']} of unit {p['unit']}: {p['files_done']} files done, "
                     f"last {p['last_file']}, {p['samples']} resource samples saved")
    text = "\n".join(lines) + "\n"
    print(text, end="")
    if a.summary_md:
        with open(a.summary_md, "a") as fh:
            fh.write(text)
    if res is None:
        return 2
    return 1 if doc["summary"]["unexplained"] or doc["summary"]["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
