#!/usr/bin/env python3
"""Tests of the sharded reference diff (scripts/courier-reference/
reference-diff.py, reference-diff-merge.py, monitor.py) with fake Poppler
tools: units join to the unsharded result; a unit killed part-way keeps
its progress and is never merged; attempts are never mixed within a
unit; units of other inputs or references are refused.

    python3 scripts/test_reference_diff.py

A process killed here is a subprocess of the test — not a lost runner,
whose last unsaved output no test can stand for.
"""
import importlib.util
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
CR = os.path.join(HERE, "courier-reference")


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), os.path.join(CR, name + ".py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


rd = load("reference-diff")
merge = load("reference-diff-merge")

# A "PDF" here is a line of settings the fake tools read.
PDFTOPPM = r"""#!/usr/bin/env python3
import os, sys, time
args = sys.argv[1:]
pdf, prefix = args[-2], args[-1]
cfg = dict(kv.split("=") for kv in open(pdf).read().split())
time.sleep(float(cfg.get("sleep", 0)))
e1 = bool(os.environ.get("FONTCONFIG_FILE"))
face = "NimbusMonoPS-Regular.otf" if e1 and cfg.get("courier") == "1" else "LiberationMono-Regular.ttf"
if cfg.get("other") == "1" and e1:
    face = "DejaVuSans.ttf"
print("Best score\n  file: \"/fonts/%s\"" % face)
for n in range(1, int(cfg["pages"]) + 1):
    px = bytes([(n * 7 + (1 if face != "LiberationMono-Regular.ttf" else 0)) % 256]) * 16
    open("%s-%d.pgm" % (prefix, n), "wb").write(b"P5\n4 4\n255\n" + px)
sys.exit(int(cfg.get("status", 0)))
"""
PDFINFO = r"""#!/usr/bin/env python3
import sys
cfg = dict(kv.split("=") for kv in open(sys.argv[-1]).read().split())
print("Pages: %s" % cfg["pages"])
"""


class Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="refdiff-test-")
        self.bin = os.path.join(self.tmp, "bin")
        os.makedirs(self.bin)
        for name, body in (("pdftoppm", PDFTOPPM), ("pdfinfo", PDFINFO)):
            p = os.path.join(self.bin, name)
            with open(p, "w") as fh:
                fh.write(body.replace("/usr/bin/env python3", sys.executable, 1))
            os.chmod(p, 0o755)
        self.e1 = os.path.join(self.tmp, "e1")
        os.makedirs(os.path.join(self.e1, "bin"))
        w = os.path.join(self.e1, "bin", "pdftoppm")
        with open(w, "w") as fh:
            fh.write(f"#!/bin/sh\nFONTCONFIG_FILE={self.e1}/fonts.conf exec {self.bin}/pdftoppm \"$@\"\n")
        os.chmod(w, 0o755)
        self.corpus = os.path.join(self.tmp, "corpus")
        os.makedirs(self.corpus)
        self.files = {}
        for i in range(10):
            name = f"f{i}" if i % 3 else f"f{i}-x"  # `a-x.pdf` sorts before `a.pdf`
            self.add(name, pages=1 + i % 3, courier=int(i in (2, 5)))
        self.env = dict(os.environ, PATH=self.bin + os.pathsep + os.environ.get("PATH", ""))
        self.env.pop("FONTCONFIG_FILE", None)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def add(self, name, manifest=True, **cfg):
        path = os.path.join(self.corpus, name + ".pdf")
        with open(path, "w") as fh:
            fh.write(" ".join(f"{k}={v}" for k, v in cfg.items()))
        if manifest:
            self.files[name] = rd.hashlib.sha256(open(path, "rb").read()).hexdigest()
        self.write_manifest()

    def write_manifest(self):
        self.manifest = os.path.join(self.tmp, "manifest.yaml")
        with open(self.manifest, "w") as fh:
            fh.write("version: 1\nfiles:\n" + "".join(f"- id: {i}\n  sha256: {h}\n" for i, h in self.files.items()))

    def unit(self, out, shard=None, chunk=None, attempt=1, wait=True):
        d = os.path.join(out, f"reference-diff-s{(shard or '1/1')[0]}-c{(chunk or '1/1')[0]}-a{attempt}")
        os.makedirs(d, exist_ok=True)
        argv = [sys.executable, os.path.join(CR, "reference-diff.py"), self.corpus, self.e1,
                "--json", os.path.join(d, "unit.json"), "--progress", os.path.join(d, "progress.jsonl"), "--jobs", "1",
                "--manifest", self.manifest]
        if shard:
            argv += ["--shard", shard, "--chunk", chunk]
        env = dict(self.env, GITHUB_SHA="c" * 40, GITHUB_RUN_ID="7", GITHUB_RUN_ATTEMPT=str(attempt))
        p = subprocess.Popen(argv, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        if wait:
            p.communicate()
        return p, d

    def merged(self, out, shards, chunks):
        j = os.path.join(self.tmp, "merged.json")
        r = subprocess.run([sys.executable, os.path.join(CR, "reference-diff-merge.py"), out, "--shards", str(shards),
                            "--chunks", str(chunks), "--json", j], capture_output=True, text=True)
        return r.returncode, json.load(open(j))


def strip(files):
    return {k: {x: y for x, y in v.items() if x != "diagnostics"} for k, v in files.items()}


class Units(Fixture):
    def test_units_join_to_the_unsharded_result(self):
        p, d = self.unit(os.path.join(self.tmp, "whole"))
        self.assertEqual(p.returncode, 0)
        whole = json.load(open(os.path.join(d, "unit.json")))
        self.assertTrue(whole["complete"])
        out = os.path.join(self.tmp, "art")
        for s in range(1, 4):
            for c in range(1, 3):
                self.unit(out, f"{s}/3", f"{c}/2")
        rc, doc = self.merged(out, 3, 2)
        self.assertEqual(rc, 0, doc["problems"])
        self.assertEqual(doc["summary"], whole["summary"])
        self.assertEqual(strip(doc["files"]), strip(whole["files"]))
        self.assertEqual(doc["summary"]["files_changed"], 2)
        # Diagnostics per file: time, each Poppler's peak RSS and output.
        diag = whole["files"]["f1"]["diagnostics"]
        self.assertEqual(set(diag), {"seconds", "e0", "e1"})
        self.assertEqual(set(diag["e0"]), {"rss_kb", "out_bytes"})

    def test_selection_partitions_the_corpus(self):
        pdfs = sorted((os.path.join(self.corpus, f) for f in os.listdir(self.corpus)),
                      key=lambda p: os.path.basename(p))
        for n, m in ((1, 1), (3, 2), (4, 3), (12, 1)):
            got = [p for s in range(1, n + 1) for c in range(1, m + 1) for p in rd.select(pdfs, (s, n), (c, m))]
            self.assertEqual(sorted(got), pdfs, (n, m))

    def test_a_killed_unit_keeps_its_progress_and_is_never_merged(self):
        self.add("slow", pages=1, sleep=30)
        out = os.path.join(self.tmp, "art")
        p, d = self.unit(out, wait=False)
        prog = os.path.join(d, "progress.jsonl")
        deadline = time.time() + 60
        while time.time() < deadline:
            lines = open(prog).read().splitlines() if os.path.exists(prog) else []
            if sum('"event": "file"' in l for l in lines) >= 3:
                break
            time.sleep(0.1)
        os.kill(p.pid, signal.SIGKILL)  # the process, not a runner
        p.communicate()
        events = [json.loads(l)["event"] for l in open(prog)]
        self.assertEqual(events[0], "start")
        self.assertGreaterEqual(events.count("file"), 3)
        self.assertNotIn("end", events)
        self.assertFalse(os.path.exists(os.path.join(d, "unit.json")))
        with open(os.path.join(d, "monitor.jsonl"), "w") as fh:
            fh.write(json.dumps({"time": 1, "mem_available_kb": 5}) + "\n" + '{"time": 2, "mem_av')  # cut short
        rc, doc = self.merged(out, 1, 1)
        self.assertEqual(rc, 2)
        self.assertFalse(doc["complete"])
        self.assertNotIn("summary", doc)
        lost = doc["incomplete_attempts"][0]
        self.assertGreaterEqual(lost["files_done"], 3)
        self.assertFalse(lost["ended"])
        self.assertEqual(lost["samples"], 1)

    def test_attempts_are_never_mixed(self):
        out = os.path.join(self.tmp, "art")
        self.unit(out, "1/2", "1/1", attempt=1)
        self.unit(out, "2/2", "1/1", attempt=1)
        # A second attempt of unit 2 that did not finish: the first is used.
        d = os.path.join(out, "reference-diff-s2-c1-a2")
        os.makedirs(d)
        open(os.path.join(d, "progress.jsonl"), "w").write('{"event": "start"}\n')
        rc, doc = self.merged(out, 2, 1)
        self.assertEqual((rc, doc["attempts"]), (0, {"1.1": 1, "2.1": 1}))
        self.assertEqual(doc["incomplete_attempts"][0]["attempt"], 2)
        # A complete second attempt replaces the first, whole.
        self.unit(out, "2/2", "1/1", attempt=3)
        rc, doc = self.merged(out, 2, 1)
        self.assertEqual((rc, doc["attempts"]), (0, {"1.1": 1, "2.1": 3}))

    def test_units_that_do_not_belong_together(self):
        out = os.path.join(self.tmp, "art")
        for s in (1, 2):
            self.unit(out, f"{s}/2", "1/1")
        rc, _ = self.merged(out, 2, 1)
        self.assertEqual(rc, 0)
        path = os.path.join(out, "reference-diff-s2-c1-a1", "unit.json")
        good = json.load(open(path))
        for change in ({"references": dict(good["references"], e1={"profile": "other"})},
                       {"corpus": dict(good["corpus"], sha256="0" * 64)},
                       {"harness_sha256": "1" * 64},
                       {"run": dict(good["run"], run_id="8")},
                       {"unit": dict(good["unit"], files=good["unit"]["files"][:-1])}):
            json.dump(dict(good, **change), open(path, "w"))
            rc, doc = self.merged(out, 2, 1)
            self.assertEqual(rc, 2, change)
            self.assertTrue(doc["problems"])
        json.dump(good, open(path, "w"))
        rc, doc = self.merged(out, 3, 1)  # a unit missing
        self.assertEqual(rc, 2)

    def test_the_corpus_is_the_manifests(self):
        out = os.path.join(self.tmp, "art")
        # A file the manifest lists that is missing — in every unit's view.
        os.remove(os.path.join(self.corpus, "f1.pdf"))
        for s in (1, 2):
            p, d = self.unit(out, f"{s}/2", "1/1")
        rc, doc = self.merged(out, 2, 1)
        self.assertEqual(rc, 2, doc)
        self.assertFalse(doc["complete"])
        # The same name and size, other bytes.
        shutil.rmtree(out)
        self.add("f1", pages=2, courier=0)  # back, as the manifest has it
        path = os.path.join(self.corpus, "f2.pdf")
        data = open(path, "rb").read()
        open(path, "wb").write(data[:-1] + (b"0" if data[-1:] != b"0" else b"1"))
        p, d = self.unit(out)
        self.assertEqual(p.returncode, 2)
        self.assertFalse(json.load(open(os.path.join(d, "unit.json")))["complete"])
        self.assertEqual(self.merged(out, 1, 1)[0], 2)
        # A file the manifest does not list is not part of the corpus.
        open(path, "wb").write(data)
        shutil.rmtree(out)
        self.add("extra", manifest=False, pages=1)
        p, d = self.unit(out)
        self.assertEqual(p.returncode, 0)
        self.assertNotIn("extra", json.load(open(os.path.join(d, "unit.json")))["files"])

    def test_an_empty_corpus_is_never_complete(self):
        self.files = {}
        self.write_manifest()
        p, d = self.unit(os.path.join(self.tmp, "art"))
        self.assertNotEqual(p.returncode, 0)
        self.assertFalse(os.path.exists(os.path.join(d, "unit.json")))

    def test_the_verdict_names_the_attempts_it_took(self):
        out = os.path.join(self.tmp, "art")
        self.unit(out, attempt=2)
        # An older attempt with other metadata, superseded: not reported.
        d = os.path.join(out, "reference-diff-s1-c1-a1")
        os.makedirs(d)
        good = json.load(open(os.path.join(out, "reference-diff-s1-c1-a2", "unit.json")))
        json.dump(dict(good, harness_sha256="9" * 64, run=dict(good["run"], attempt="1")), open(os.path.join(d, "unit.json"), "w"))
        rc, doc = self.merged(out, 1, 1)
        self.assertEqual((rc, doc["attempts"]), (0, {"1.1": 2}))
        self.assertEqual((doc["harness_sha256"], doc["run"]["attempt"]), (good["harness_sha256"], "2"))

    def test_an_unexplained_change_fails(self):
        self.add("odd", pages=1, other=1)
        out = os.path.join(self.tmp, "art")
        p, _ = self.unit(out)
        self.assertEqual(p.returncode, 1)
        rc, doc = self.merged(out, 1, 1)
        self.assertEqual(rc, 1)
        self.assertEqual(doc["summary"]["unexplained"], ["odd"])

    def test_a_timeout_is_not_compared(self):
        self.add("hang", pages=1, sleep=5)
        old, rd.TIMEOUT = rd.TIMEOUT, 1
        saved = dict(os.environ)
        os.environ.update(self.env)
        try:
            fid, r = rd.one((os.path.join(self.corpus, "hang.pdf"), self.e1, 96))
        finally:
            rd.TIMEOUT = old
            os.environ.clear()
            os.environ.update(saved)
        self.assertEqual((fid, r["status"], r["coverage"]), ("hang", "timeout", "none"))
        self.assertTrue(r["diagnostics"]["e0"]["timeout"])


class IsolationTrace(unittest.TestCase):
    """isolation-probe.py's reading of strace output: every open form, a
    relative path, an arbitrary configuration file outside the reference."""

    def test_trace(self):
        ip = load("isolation-probe")
        with tempfile.TemporaryDirectory() as d:
            root = os.path.join(os.path.realpath(d), "ref")
            log = os.path.join(d, "s.log")
            open(log, "w").write(
                f'12 openat(AT_FDCWD, "{root}/fonts/a.ttf", O_RDONLY) = 3\n'
                '12 open("/tmp/rogue.conf", O_RDONLY) = 4\n'
                '12 openat(AT_FDCWD, "rel/x.otf", O_RDONLY) = 5\n'
                '12 openat2(AT_FDCWD, "/etc/fonts/local.conf", {flags=O_RDONLY}, 24) = 6\n'
                '12 openat(AT_FDCWD, "/usr/share/fontconfig/conf.avail/10-a.conf", O_RDONLY) = 7\n'
                '12 openat(AT_FDCWD, "/usr/lib/libfoo.so.1", O_RDONLY) = 8\n'
                '12 openat(AT_FDCWD, "/usr/share/fonts/missing.ttf", O_RDONLY) = -1 ENOENT (No such file)\n')
            opened, parsed, attempted = ip.outside_font_access(log, root, cwd="/work")
            self.assertEqual(opened, sorted(["/tmp/rogue.conf", "/work/rel/x.otf", "/etc/fonts/local.conf"]))
            self.assertEqual(parsed, ["/usr/share/fontconfig/conf.avail/10-a.conf"])
            self.assertEqual(attempted, ["/usr/share/fonts/missing.ttf"])
            # Described-only by fc-conflist (`-`): counted apart, not opened.
            opened, parsed, _ = ip.outside_font_access(log, root, parse_only={"/etc/fonts/local.conf"}, cwd="/work")
            self.assertNotIn("/etc/fonts/local.conf", opened)


class Monitor(unittest.TestCase):
    def test_samples_are_saved_as_taken(self):
        with tempfile.TemporaryDirectory() as d:
            out = os.path.join(d, "m.jsonl")
            p = subprocess.Popen([sys.executable, os.path.join(CR, "monitor.py"), out, "--interval", "0.1", "--path", d])
            time.sleep(1)
            p.send_signal(signal.SIGKILL)
            p.wait()
            rows = [json.loads(l) for l in open(out)]
            self.assertGreaterEqual(len(rows), 3)
            self.assertIn(d, rows[0]["disk_free_bytes"])
            for r in rows:
                self.assertTrue(all(set(t) == {"name", "rss_kb"} for t in r["top_rss"]))


if __name__ == "__main__":
    unittest.main()
