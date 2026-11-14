#!/usr/bin/env python3
"""Tests of scripts/advisory-history.py and of the renderer baselines'
protection (pr.yml compat-smoke). Fake CLIs, throwaway git repositories
and hand-made crates stand in for the published versions: no network, no
corpus, no cargo. Every way a cell or a version can go wrong must end in
a problem — never in LEAK being "reproduced" or the candidate "safe" by
accident.

    python3 scripts/test_advisory_history.py
"""
import hashlib
import importlib.util
import io
import json
import os
import shutil
import subprocess
import sys
import tarfile
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts" / file)
    mod = importlib.util.module_from_spec(spec)
    argv, sys.argv = sys.argv, [sys.argv[0]]
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.argv = argv
    return mod


ah = load("advisory_history", "advisory-history.py")
car = load("check_advisory_regressions", "check-advisory-regressions.py")
vrend = load("verify_renderers", "verify-renderers.py")

GHSA = "GHSA-test-test-test"
ADV = {GHSA: {"affected": [">= 0.6.0", "< 0.8.0"], "patched": "0.8.0"}}
VERSIONS = ["0.5.0", "0.6.0", "0.7.0", "0.8.0"]


def rows(*names, expect="safe-bytes:X"):
    return [(n, GHSA, "redact --page 1 --rect 1 1 2 2", expect) for n in names]


def cells(**per_version):
    return {k.replace("_", "."): {"state": v} for k, v in per_version.items()}


def expected(name, **per_version):
    return {"expected": {name: {k.replace("_", "."): v for k, v in per_version.items()}}}


GOOD = dict(v0_5_0="NA", v0_6_0="LEAK", v0_7_0="LEAK", v0_8_0="REFUSED", candidate="REFUSED")


def norm(d):
    return {k.replace("v0_", "0_"): v for k, v in d.items()}


def decide(observed, want):
    return ah.decide(rows("a.pdf"), {"a.pdf": cells(**norm(observed))}, expected("a.pdf", **norm(want)), ADV, VERSIONS)


class Decide(unittest.TestCase):
    def test_as_expected(self):
        problems, ranges, untestable = decide(GOOD, GOOD)
        self.assertEqual(problems, [])
        self.assertEqual(ranges[GHSA]["0.6.0"], "vulnerable")
        self.assertEqual(untestable, [])

    def test_5_old_version_does_not_leak(self):
        problems, _, _ = decide(dict(GOOD, v0_7_0="REMOVED"), GOOD)
        self.assertIn("not_reproduced:a.pdf:0.7.0:LEAK->REMOVED", problems)
        self.assertIn(f"range_not_reproduced:{GHSA}:0.7.0", problems)

    def test_6_candidate_leaks(self):
        problems, _, _ = decide(dict(GOOD, candidate="LEAK"), GOOD)
        self.assertIn("candidate_unsafe:a.pdf:LEAK", problems)
        self.assertIn(f"range_outside:{GHSA}:candidate", problems)

    def test_6_a_leak_outside_the_range(self):
        problems, _, _ = decide(dict(GOOD, v0_8_0="LEAK"), dict(GOOD, v0_8_0="LEAK"))
        self.assertIn(f"range_outside:{GHSA}:0.8.0", problems)

    def test_10_unexpected_refusal_and_success(self):
        want = dict(GOOD, candidate="REMOVED")
        self.assertIn("unexpected_refusal:a.pdf:candidate:REMOVED->REFUSED", decide(GOOD, want)[0])
        self.assertIn("unexpected_success:a.pdf:candidate:REFUSED->REMOVED", decide(want, GOOD)[0])

    def test_errors_are_never_a_state_to_reach(self):
        for state in ("CRASH", "TIMEOUT", "ERROR"):
            with self.subTest(state):
                problems, _, _ = decide(dict(GOOD, v0_6_0=state), GOOD)
                self.assertIn(f"not_observed:a.pdf:0.6.0:{state}", problems)
                problems, _, _ = decide(dict(GOOD, candidate=state), GOOD)
                self.assertIn(f"candidate_unsafe:a.pdf:{state}", problems)

    def test_1_a_build_that_failed(self):
        # A version that did not build has ERROR in every cell.
        problems, _, _ = decide(dict(GOOD, v0_6_0="ERROR"), GOOD)
        self.assertTrue(any(p.startswith("not_observed") for p in problems))
        self.assertEqual(ah.record_problems("0.6.0", {"status": "error", "problem": "build_failed"}, {}),
                         ["version:0.6.0:build_failed"])

    def test_3_unexpected_features_or_toolchain(self):
        cfg = {"toolchain": "1.92", "features": []}
        rec = {"status": "built", "version": "0.6.0", "tag": "v0.6.0", "source": "crates.io", "toolchain": "1.92",
               "features": []}
        self.assertEqual(ah.record_problems("0.6.0", rec, cfg), [])
        self.assertIn("version:0.6.0:features",
                      ah.record_problems("0.6.0", dict(rec, features=["cjk-fallback"]), cfg))
        self.assertIn("version:0.6.0:toolchain", ah.record_problems("0.6.0", dict(rec, toolchain="nightly"), cfg))
        self.assertIn("version:0.6.0:record", ah.record_problems("0.6.0", dict(rec, source="git"), cfg))
        self.assertIn("version:0.6.0:record", ah.record_problems("0.6.0", dict(rec, version="0.8.0"), cfg))

    def test_4_inputs_missing_or_unknown(self):
        problems, _, _ = ah.decide(rows("b.pdf"), {"b.pdf": cells(**norm(GOOD))}, expected("a.pdf", **norm(GOOD)),
                                   ADV, VERSIONS)
        self.assertIn("input_missing:a.pdf", problems)
        self.assertIn("input_new:b.pdf", problems)

    def test_untestable(self):
        problems, ranges, untestable = decide(dict(GOOD, v0_6_0="NA"), dict(GOOD, v0_6_0="NA"))
        self.assertIn(f"{GHSA}:0.6.0", untestable)
        self.assertEqual(ranges[GHSA]["0.6.0"], "untestable")
        # An advisory without inputs passes only when history.json says why.
        adv = {**ADV, "GHSA-none-none-none": {"affected": ["< 0.6.0"], "patched": "0.6.0"}}
        p, _, _ = ah.decide(rows("a.pdf"), {"a.pdf": cells(**norm(GOOD))}, expected("a.pdf", **norm(GOOD)), adv,
                            VERSIONS)
        self.assertIn("range_no_inputs:GHSA-none-none-none", p)
        adv["GHSA-none-none-none"]["untestable"] = "API only"
        p, _, u = ah.decide(rows("a.pdf"), {"a.pdf": cells(**norm(GOOD))}, expected("a.pdf", **norm(GOOD)), adv,
                            VERSIONS)
        self.assertEqual((p, u), ([], ["GHSA-none-none-none:all"]))

    def test_kept_inputs_are_not_range_evidence(self):
        r = rows("a.pdf") + [("k.pdf", GHSA, "redact --page 1", "kept-bytes:Y")]
        c = {"a.pdf": cells(**norm(GOOD)), "k.pdf": cells(**norm({k: "KEPT" for k in GOOD}))}
        e = {"expected": {"a.pdf": norm(GOOD), "k.pdf": norm({k: "KEPT" for k in GOOD})}}
        e = {"expected": {n: {k.replace("_", "."): v for k, v in m.items()} for n, m in e["expected"].items()}}
        self.assertEqual(ah.decide(r, c, e, ADV, VERSIONS)[0], [])


class Judge(unittest.TestCase):
    """judge_case with fake CLIs."""

    def fake(self, body):
        d = Path(tempfile.mkdtemp())
        f = d / "kasane"
        f.write_text("#!/usr/bin/env python3\nimport os, sys, time, signal\n"
                     "if len(sys.argv) == 1:\n    print('usage: kasane-pdf redact | render')\n    sys.exit(2)\n"
                     "out = sys.argv[sys.argv.index('--output') + 1]\n" + body)
        f.chmod(0o755)
        src = d / "in.pdf"
        src.write_bytes(b"%PDF-1.4\n")
        return str(f), str(src), d

    def judge(self, body, expect="safe-bytes:SECRETX", timeout=20):
        k, src, d = self.fake(body)
        return car.judge_case(k, src, "redact --page 1 --rect 1 1 2 2", expect, str(d), car.usage_of(k),
                              timeout=timeout)["state"]

    def test_8_timeout(self):
        self.assertEqual(self.judge("time.sleep(30)", timeout=1), "TIMEOUT")

    def test_9_success_without_output(self):
        self.assertEqual(self.judge("sys.exit(0)"), "CRASH")

    def test_a_refusal_is_its_own_state(self):
        self.assertEqual(self.judge("sys.stderr.write('error [unsupported feature]: x'); sys.exit(1)"), "REFUSED")
        # A refusal that writes the output is not a refusal.
        self.assertEqual(self.judge("open(out, 'w').write('x'); sys.stderr.write('error [x]: y'); sys.exit(1)"),
                         "CRASH")
        self.assertEqual(self.judge("sys.exit(1)"), "CRASH")

    def test_7_witness_failure(self):
        if shutil.which("qpdf") is None:
            self.skipTest("qpdf not installed")
        self.assertEqual(self.judge("open(out, 'w').write('not a pdf'); sys.exit(0)"), "ERROR")

    def test_7_a_renderer_witness_failing_is_an_error(self):
        class FakeVR:
            RENDERERS = ("poppler",)

            @staticmethod
            def text(rd, pdf):
                return None

            @staticmethod
            def norm(t):
                return t
        reasons, error = ah.witnesses(FakeVR, "in.pdf", "out.pdf", "safe-text:S", "redact --rect 1 1 2 2")
        self.assertEqual(error, "text_failed:mupdf")


class Crates(unittest.TestCase):
    """A package checked against its tag, in a throwaway repository."""

    def setUp(self):
        self.repo = Path(tempfile.mkdtemp())
        g = lambda *a: subprocess.run(["git", "-C", str(self.repo), *a], check=True, capture_output=True, text=True)
        g("init", "-q", "-b", "main")
        g("config", "user.email", "t@example.invalid")
        g("config", "user.name", "t")
        (self.repo / "src").mkdir()
        (self.repo / "src" / "lib.rs").write_text("pub fn f() {}\n")
        (self.repo / "Cargo.toml").write_text("[package]\nname = 'kasane-pdf'\nversion = '0.6.0'\n")
        g("add", "-A")
        g("commit", "-qm", "v")
        g("tag", "v0.6.0")
        self.commit = g("rev-parse", "HEAD").stdout.strip()

    def crate(self, files, sha=None, link=False):
        buf = io.BytesIO()
        with tarfile.open(fileobj=buf, mode="w:gz") as tf:
            files = {".cargo_vcs_info.json": json.dumps({"git": {"sha1": sha or self.commit}}).encode(), **files}
            for name, data in files.items():
                info = tarfile.TarInfo(f"kasane-pdf-0.6.0/{name}")
                info.size = len(data)
                tf.addfile(info, io.BytesIO(data))
            if link:
                info = tarfile.TarInfo("kasane-pdf-0.6.0/evil")
                info.type = tarfile.SYMTYPE
                info.linkname = "/etc/passwd"
                tf.addfile(info)
        return buf.getvalue()

    def check(self, data):
        return ah.verify_crate_against_tag("0.6.0", data, self.commit, repo=self.repo)[0]

    def good(self):
        return {"src/lib.rs": b"pub fn f() {}\n", "Cargo.toml.orig": (self.repo / "Cargo.toml").read_bytes(),
                "Cargo.toml": b"# normalized\n", "Cargo.lock": b"# generated\n"}

    def test_the_tags_package(self):
        self.assertEqual(self.check(self.crate(self.good())), [])

    def test_2_a_file_differs_from_the_tag(self):
        self.assertIn("file_differs_from_tag", self.check(self.crate(dict(self.good(), **{"src/lib.rs": b"evil\n"}))))
        self.assertIn("file_not_in_tag", self.check(self.crate(dict(self.good(), **{"src/extra.rs": b"x\n"}))))
        self.assertIn("file_differs_from_tag",
                      self.check(self.crate(dict(self.good(), **{"Cargo.toml.orig": b"[package]\n"}))))

    def test_2_another_commit(self):
        self.assertIn("vcs_not_tag", self.check(self.crate(self.good(), sha="0" * 40)))

    def test_2_a_link_in_the_package(self):
        self.assertIn("crate_entry", self.check(self.crate(self.good(), link=True)))

    def test_11_a_cached_binary_changed(self):
        cache = Path(tempfile.mkdtemp())
        (cache / "bin" / "0.6.0").mkdir(parents=True)
        (cache / "records").mkdir()
        b = cache / "bin" / "0.6.0" / "kasane-pdf"
        b.write_bytes(b"binary")
        cfg = {"toolchain": "1.92", "features": []}
        rec = {"status": "built", "version": "0.6.0", "toolchain": "1.92", "features": [],
               "binary_sha256": hashlib.sha256(b"binary").hexdigest()}
        (cache / "records" / "0.6.0.json").write_text(json.dumps(rec))
        self.assertEqual(ah.build_version("0.6.0", cfg, cache, cache / "log")["status"], "built")
        b.write_bytes(b"other binary")
        got = ah.build_version("0.6.0", cfg, cache, cache / "log")
        self.assertEqual((got["status"], got["problem"]), ("error", "binary_digest"))


class Private(unittest.TestCase):
    def test_15_public_json_names_no_private_input(self):
        secret = "privateclientname"
        r = [(f"{secret}.pdf", GHSA, f"redact --page 1 --rect 61.5 712.25 3 4", f"safe-text:{secret}")]
        c = {f"{secret}.pdf": cells(**norm(GOOD))}
        text = json.dumps(ah.private_summary(r, c, ["mismatch:" + secret]))
        for needle in (secret, "61.5", "712.25", "redact"):
            self.assertNotIn(needle, text)
        self.assertEqual(json.loads(text)["inputs"], 1)

    def test_private_inputs_refused_in_actions(self):
        env = dict(os.environ, GITHUB_ACTIONS="true")
        r = subprocess.run([sys.executable, str(ROOT / "scripts" / "advisory-history.py"), "run", "--dump", "/x",
                            "--candidate", "/x", "--json", "/tmp/x.json", "--private-inputs", "/y",
                            "--private-json", "/tmp/y.json"], capture_output=True, text=True, env=env)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("never run in GitHub Actions", r.stdout + r.stderr)


class UpdateExpected(unittest.TestCase):
    def run_update(self, results, old, *flags):
        d = Path(tempfile.mkdtemp())
        (d / "r.json").write_text(json.dumps(results))
        (d / "e.json").write_text(json.dumps(old))
        r = subprocess.run([sys.executable, str(ROOT / "scripts" / "advisory-history.py"), "update-expected",
                            "--results", str(d / "r.json"), "--expected", str(d / "e.json"), *flags],
                           capture_output=True, text=True)
        return r.returncode, json.loads((d / "e.json").read_text()), r.stdout

    def results(self, problems=(), **states):
        return {"problems": list(problems),
                "inputs": {"a.pdf": {"cells": {k.replace("_", "."): {"state": v} for k, v in states.items()}}}}

    def test_failures_are_not_written_as_expectations(self):
        for p in ("candidate_unsafe:a.pdf:LEAK", f"range_not_reproduced:{GHSA}:0.6.0", "not_observed:a.pdf:0.6.0:CRASH",
                  "version:0.6.0:build_failed", f"range_outside:{GHSA}:0.8.0"):
            with self.subTest(p):
                code, doc, out = self.run_update(self.results([p], **norm(GOOD)), {"expected": {}})
                self.assertEqual((code, doc["expected"]), (1, {}))

    def test_a_lost_leak_needs_accept(self):
        old = {"expected": {"a.pdf": {k.replace("_", "."): v for k, v in norm(GOOD).items()}}}
        new = self.results(**norm(dict(GOOD, v0_6_0="REMOVED")))
        code, doc, out = self.run_update(new, old)
        self.assertEqual(code, 1)
        self.assertEqual(doc, old)
        self.assertIn("a.pdf 0.6.0: LEAK -> REMOVED", out)
        code, doc, out = self.run_update(new, old, "--accept")
        self.assertEqual((code, doc["expected"]["a.pdf"]["0.6.0"]), (0, "REMOVED"))


class Baselines(unittest.TestCase):
    def test_12_another_platforms_baseline(self):
        self.assertIsNone(vrend.baseline_mismatch({"platform": "linux-x86_64"}, "linux-x86_64"))
        self.assertIn("baseline of darwin-arm64", vrend.baseline_mismatch({"platform": "darwin-arm64"}, "linux-x86_64"))
        self.assertIn("baseline of", vrend.baseline_mismatch({}, "linux-x86_64"))
        self.assertIn("no baseline", vrend.baseline_mismatch(None, "linux-x86_64"))

    def test_the_committed_baselines_say_their_platform(self):
        for p in (ROOT / "compat" / "renderers").glob("baseline-*.json"):
            with self.subTest(p.name):
                d = json.loads(p.read_text())
                self.assertEqual(f"baseline-{d['platform']}.json", p.name)
                self.assertTrue(d.get("reason"))
                self.assertTrue(all(d["tools"].values()))

    def test_14_a_baseline_never_records_a_safety_failure(self):
        problems = ["leak_ink:x:mupdf", "loss_text:y:pdfjs", "crash:z", "outcome_not_expected:w", "loss_drawing:v:poppler",
                    "case_not_in_baseline:u"]
        self.assertEqual(vrend.safety_problems(problems), problems[:4])

    def test_14_increases_need_acceptance(self):
        old = {"platform": "p", "tools": {"poppler": "1", "mupdf": "1", "pdfjs": "1"},
               "cases": {"a": {"outcome": "ok", "renderers": {"mupdf": {"changed": 2}}}}}
        same = json.loads(json.dumps(old))
        self.assertEqual(vrend.baseline_diff(old, same), ([], []))
        more = json.loads(json.dumps(old))
        more["cases"]["a"]["renderers"]["mupdf"]["changed"] = 3
        lines, accept = vrend.baseline_diff(old, more)
        self.assertEqual(accept, ["a:mupdf"])
        fewer = json.loads(json.dumps(old))
        fewer["cases"]["a"]["renderers"]["mupdf"]["changed"] = 1
        self.assertEqual(vrend.baseline_diff(old, fewer)[1], [])
        refused = json.loads(json.dumps(old))
        refused["cases"]["a"]["outcome"] = "refused"
        self.assertEqual(vrend.baseline_diff(old, refused)[1], ["a"])
        bumped = json.loads(json.dumps(old))
        bumped["tools"]["mupdf"] = "2"
        self.assertTrue(any("version mupdf" in l for l in vrend.baseline_diff(old, bumped)[0]))


class Committed(unittest.TestCase):
    """The committed history.json is consistent with itself."""

    def test_history(self):
        d = json.loads((ROOT / "compat" / "advisories" / "history.json").read_text())
        for v in d["versions"]:
            self.assertRegex(v, r"^\d+\.\d+\.\d+$")
        for name, row in d["expected"].items():
            with self.subTest(name):
                self.assertEqual(set(row), set(d["versions"]) | {"candidate"})
                self.assertIn(row["candidate"], ah.SAFE_CANDIDATE)
                for state in row.values():
                    self.assertIn(state, car.STATES)
        for g, a in d["advisories"].items():
            self.assertTrue(a["affected"] and a["patched"], g)


if __name__ == "__main__":
    unittest.main(verbosity=1)
