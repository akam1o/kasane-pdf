#!/usr/bin/env python3
"""Tests for the compatibility-analysis tools and their published output
(docs/compatibility-report.md). No corpus needed.

1. The published files in compat/analysis/ carry counts only: no
   corpus id (as a key or a string value), no SHA-256, no file name.
2. scripts/compat-summary.py counts strict outcomes the way the runner
   rates them (a recovered document's downstream stages are skip) and
   does not mistake an F1 of 0.0 for a missing value.
3. scripts/diag-render-blockers.py counts a file whose strict parse
   failed as a blocker even when the recovered document rendered.

    python3 scripts/test_compat_analysis.py
"""
import glob
import importlib.util
import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# Words of the tools' own vocabulary (feature tags, profile keys) that
# happen to be corpus ids as well.
VOCABULARY = {"images", "devicen"}


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), os.path.join(ROOT, "scripts", name + ".py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


def strings(o):
    if isinstance(o, dict):
        for k, v in o.items():
            yield k
            yield from strings(v)
    elif isinstance(o, list):
        for v in o:
            yield from strings(v)
    elif isinstance(o, str):
        yield o


def test_published_files_hold_no_ids():
    ids = set(re.findall(r"^- id:\s*(\S+)", open(os.path.join(ROOT, "compat", "manifest.yaml")).read(), re.M))
    assert len(ids) > 400, "manifest ids not found"
    files = glob.glob(os.path.join(ROOT, "compat", "analysis", "*.json"))
    assert files, "no published analysis"
    for p in files:
        text = open(p).read()
        assert not re.search(r"[0-9a-f]{64}", text), f"{p}: a SHA-256"
        assert ".pdf" not in text, f"{p}: a file name"
        leaked = (set(strings(json.loads(text))) & ids) - VOCABULARY
        assert not leaked, f"{p}: corpus ids {sorted(leaked)[:3]}"


def stage(cls):
    return {"class": cls}


def synthetic_run():
    def f(fid, parse, render, text, recovered=False, f1=None, f1a=None, ours=0, ref=0):
        return {"id": fid, "expect": "pass", "verdict": "pass" if render == "pass" else "error", "recovered": recovered,
                "parse": stage(parse), "render": stage(render), "text": stage(text), "rewrite": stage("pass"),
                "recover": stage("pass" if recovered else "skip"), "render_recovered": stage("skip"),
                "features": [], "profile": {},
                "metrics": {"render_ssim": 1.0, "render_pages": [], "text_char_f1": f1, "text_char_f1_with_annots": f1a,
                            "text_chars_ours": ours, "text_chars_ref": ref}}
    return {"files": [
        f("a", "pass", "pass", "pass", f1=0.0, f1a=1.0, ours=0, ref=10),   # F1 0.0, annotations recover it
        f("b", "error", "pass", "pass", recovered=True, f1=0.0, f1a=1.0),  # recovered: not a strict pass
        f("c", "pass", "error", "skip"),
    ]}


def test_summary_counts_strict_outcomes():
    with tempfile.TemporaryDirectory() as t:
        paths = {k: os.path.join(t, k + ".json") for k in ("run", "diag", "attribution", "unicode", "baseline", "out")}
        json.dump(synthetic_run(), open(paths["run"], "w"))
        json.dump({"files": []}, open(paths["diag"], "w"))
        json.dump({"files": []}, open(paths["attribution"], "w"))
        json.dump([], open(paths["unicode"], "w"))
        json.dump({"files": {}}, open(paths["baseline"], "w"))
        subprocess.run([sys.executable, os.path.join(ROOT, "scripts", "compat-summary.py"),
                        *[x for k, p in paths.items() for x in (f"--{k}", p)]], check=True, capture_output=True)
        out = json.load(open(paths["out"]))
    assert out["stages_expect_pass"]["render"] == {"pass": 1, "skip": 1, "error": 1}, out["stages_expect_pass"]
    assert out["text_f1"]["n"] == 1
    assert out["text_f1_with_annotations_recovers"] == 1
    assert out["text_empty_with_reference_text"] == 1


def test_diag_counts_recovered_render_as_blocker():
    with tempfile.TemporaryDirectory() as t:
        run = synthetic_run()
        for f in run["files"]:
            f["parse"]["detail"] = "no startxref" if f["id"] == "b" else ""
            f["render"]["detail"] = "page 1: unknown operator 'zz' at byte 3" if f["id"] == "c" else ""
        p = os.path.join(t, "run-x.json")
        json.dump(run, open(p, "w"))
        subprocess.run([sys.executable, os.path.join(ROOT, "scripts", "diag-render-blockers.py"), p], check=True,
                       capture_output=True)
        rows = json.load(open(os.path.join(t, "diag-render-blockers-run-x.json")))["files"]
    assert sorted(r["id"] for r in rows) == ["b", "c"], rows
    assert {r["id"]: r["stage"] for r in rows} == {"b": "parse", "c": "render"}


def main():
    tests = [v for k, v in sorted(globals().items()) if k.startswith("test_")]
    for t in tests:
        t()
        print(f"ok  {t.__name__}")
    print(f"test_compat_analysis: {len(tests)} passed")


if __name__ == "__main__":
    main()
