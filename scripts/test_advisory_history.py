#!/usr/bin/env python3
"""Tests of scripts/advisory-history.py and of the renderer baselines'
protection (pr.yml compat-smoke). Fake CLIs, throwaway git repositories
and hand-made crates stand in for the published versions: no network, no
corpus, no cargo. Every way a cell or a version can go wrong must end in
a problem — never in LEAK being "reproduced" or the candidate "safe" by
accident.

    python3 scripts/test_advisory_history.py
"""
import contextlib
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
        import platform as _p
        rec = {"status": "built", "version": "0.6.0", "tag": "v0.6.0", "source": "crates.io", "toolchain": "1.92",
               "features": [], "builder": ah.BUILDER, "platform": f"{_p.system()}-{_p.machine()}",
               "crate_sha256": "a" * 64, "commit": "b" * 40, "rustc_commit": "c" * 40}
        self.assertEqual(ah.record_problems("0.6.0", rec, cfg), [])
        for k, v in (("platform", "WrongOS-WrongArch"), ("builder", 1), ("crate_sha256", None), ("commit", None)):
            self.assertIn("version:0.6.0:provenance", ah.record_problems("0.6.0", dict(rec, **{k: v}), cfg), k)
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

    def test_2_errors_are_never_learned(self):
        # An expected CRASH (or a new input's ERROR) is still a problem.
        for state in ("CRASH", "TIMEOUT", "ERROR"):
            problems, _, _ = decide(dict(GOOD, v0_6_0=state), dict(GOOD, v0_6_0=state))
            self.assertIn(f"not_observed:a.pdf:0.6.0:{state}", problems)
        r = rows("a.pdf", "b.pdf")
        c = {"a.pdf": cells(**norm(GOOD)), "b.pdf": cells(**norm(dict(GOOD, v0_6_0="ERROR")))}
        problems, _, _ = ah.decide(r, c, expected("a.pdf", **norm(GOOD)), ADV, VERSIONS)
        self.assertIn("not_observed:b.pdf:0.6.0:ERROR", problems)

    def test_3_inputs_of_an_advisory_without_a_range(self):
        r = rows("a.pdf") + [("x.pdf", "GHSA-unkn-own0-0000", "redact --page 1", "safe-bytes:X")]
        c = {"a.pdf": cells(**norm(GOOD)), "x.pdf": cells(**norm(GOOD))}
        e = {"expected": {n: {k.replace("_", "."): v for k, v in norm(GOOD).items()} for n in ("a.pdf", "x.pdf")}}
        self.assertIn("advisory_unknown:x.pdf:GHSA-unkn-own0-0000", ah.decide(r, c, e, ADV, VERSIONS)[0])

    def test_4_every_published_version_is_covered(self):
        index = [{"vers": v} for v in ("0.5.0", "0.6.0", "0.7.0")] + [{"vers": "0.4.0", "yanked": True}]
        self.assertEqual(ah.coverage_problems({"versions": ["0.5.0", "0.6.0", "0.7.0"]}, index), [])
        self.assertEqual(ah.coverage_problems({"versions": ["0.5.0", "0.6.0"]}, index), ["version_not_covered:0.7.0"])
        self.assertEqual(ah.coverage_problems({"versions": ["0.5.0", "0.6.0"], "excluded_versions": {"0.7.0": "why"}},
                                              index), [])
        self.assertEqual(ah.coverage_problems({"versions": ["0.5.0", "0.6.0", "0.7.0", "0.9.0"]}, index),
                         ["version_not_published:0.9.0"])
        for excl in ({"0.7.0": ""}, {"0.7.0": None}):
            self.assertIn("version_not_covered:0.7.0", ah.coverage_problems({"versions": ["0.5.0", "0.6.0"],
                                                                             "excluded_versions": excl}, index))
        self.assertEqual(ah.coverage_problems({"versions": [], "excluded_versions": ["0.7.0"]}, index),
                         ["excluded_versions_malformed"])
        self.assertIn("excluded_and_tested:0.7.0", ah.coverage_problems(
            {"versions": ["0.5.0", "0.6.0", "0.7.0"], "excluded_versions": {"0.7.0": "why"}}, index))

    def test_kept_inputs_are_not_range_evidence(self):
        r = rows("a.pdf") + [("k.pdf", GHSA, "redact --page 1", "kept-bytes:Y")]
        c = {"a.pdf": cells(**norm(GOOD)), "k.pdf": cells(**norm({k: "KEPT" for k in GOOD}))}
        e = {"expected": {"a.pdf": norm(GOOD), "k.pdf": norm({k: "KEPT" for k in GOOD})}}
        e = {"expected": {n: {k.replace("_", "."): v for k, v in m.items()} for n, m in e["expected"].items()}}
        self.assertEqual(ah.decide(r, c, e, ADV, VERSIONS)[0], [])


    def test_quiet_inputs_reproduce_by_what_was_printed(self):
        # A `quiet` input's LEAK counts for the range only when the CLI
        # printed the marker; one that accepted the input does not show it.
        r = rows("q.pdf", expect="quiet:M")
        want = expected("q.pdf", **norm(GOOD))
        printed = {k: (dict(v, reasons=["printed"]) if v["state"] == "LEAK" else v)
                   for k, v in cells(**norm(GOOD)).items()}
        self.assertEqual(ah.decide(r, {"q.pdf": printed}, want, ADV, VERSIONS)[0], [])
        accepted = {k: (dict(v, reasons=["accepted"]) if v["state"] == "LEAK" else v)
                    for k, v in cells(**norm(GOOD)).items()}
        p = ah.decide(r, {"q.pdf": accepted}, want, ADV, VERSIONS)[0]
        self.assertIn(f"range_not_reproduced:{GHSA}:0.6.0", p)
        self.assertIn(f"range_not_reproduced:{GHSA}:0.7.0", p)
        # Other expectations: a LEAK counts as before, reasons or not.
        self.assertEqual(ah.decide(rows("a.pdf"), {"a.pdf": cells(**norm(GOOD))}, expected("a.pdf", **norm(GOOD)),
                                   ADV, VERSIONS)[0], [])

    def test_quiet_cells_keep_their_reason(self):
        saved = ah.car.judge_case
        try:
            for reason in ("printed", "accepted"):
                ah.car.judge_case = lambda *a, reason=reason, **k: {"state": "LEAK", "output": None, "reason": reason}
                self.assertEqual(ah.judge_cell(None, "k", "s", "redact", "quiet:M", ""),
                                 {"state": "LEAK", "reasons": [reason]})
        finally:
            ah.car.judge_case = saved

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
        # A `fast` input that runs past the limit is the unbounded render
        # the advisory is about; a refused render is REFUSED, not FAST.
        self.assertEqual(self.judge("time.sleep(30)", expect="fast", timeout=1), "SLOW")
        self.assertEqual(self.judge("sys.stderr.write('error [x]: y'); sys.exit(1)", expect="fast"), "REFUSED")

    def test_quiet_refusals(self):
        # Refused, the marker printed nowhere: REFUSED.
        refuse = "sys.stderr.write('error [x]: {}'); sys.exit(1)"
        self.assertEqual(self.judge(refuse.format("y"), expect="quiet:MARKQ"), "REFUSED")
        # The marker on standard error or standard output: LEAK.
        self.assertEqual(self.judge(refuse.format("y MARKQ"), expect="quiet:MARKQ"), "LEAK")
        self.assertEqual(self.judge("print('MARKQ'); " + refuse.format("x"), expect="quiet:MARKQ"), "LEAK")
        # Accepted what it must refuse: LEAK, whatever it says.
        self.assertEqual(self.judge("open(out, 'w').write('x'); sys.exit(0)", expect="quiet:MARKQ"), "LEAK")
        # Not a refusal (an output written, no error line): never REFUSED.
        self.assertEqual(self.judge("open(out, 'w').write('x'); sys.stderr.write('error [x]: y'); sys.exit(1)",
                                    expect="quiet:MARKQ"), "CRASH")
        self.assertEqual(self.judge("sys.exit(1)", expect="quiet:MARKQ"), "CRASH")
        # Other expectations never read the message.
        self.assertEqual(self.judge(refuse.format("SECRETX"), expect="safe-bytes:SECRETX"), "REFUSED")

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

    def test_11_a_failed_witness_beside_a_leak_is_an_error(self):
        saved = ah.car.judge_case, ah.witnesses
        ah.car.judge_case = lambda *a, **k: {"state": "LEAK", "output": "/o.pdf"}
        ah.witnesses = lambda *a: ([], "text_failed:mupdf")
        try:
            cell = ah.judge_cell(None, "k", "s", "redact --rect 1 1 2 2", "safe-text:S", "")
        finally:
            ah.car.judge_case, ah.witnesses = saved
        self.assertEqual(cell["state"], "ERROR")

    def test_7_expected_candidate_states_in_the_fast_gate(self):
        k, src, d = self.fake("sys.stderr.write('error [x]: y'); sys.exit(1)")
        dump = d / "dump"
        dump.mkdir()
        shutil.copyfile(src, dump / "a.pdf")
        (dump / "manifest-a.tsv").write_text("a.pdf\tGHSA-t\tredact --page 1 --rect 1 1 2 2\tsafe-text:S\n")
        exp = d / "e.json"
        cmd = [sys.executable, str(ROOT / "scripts" / "check-advisory-regressions.py"), k, str(dump), "--expect-safe"]
        self.assertEqual(subprocess.run(cmd, capture_output=True).returncode, 0)
        exp.write_text(json.dumps({"expected": {"a.pdf": {"candidate": "REMOVED"}}}))
        r = subprocess.run(cmd + ["--expected", str(exp)], capture_output=True, text=True)
        self.assertEqual(r.returncode, 1, r.stdout)
        exp.write_text(json.dumps({"expected": {"a.pdf": {"candidate": "REFUSED"}}}))
        self.assertEqual(subprocess.run(cmd + ["--expected", str(exp)], capture_output=True).returncode, 0)
        exp.write_text(json.dumps({"expected": {}}))
        self.assertEqual(subprocess.run(cmd + ["--expected", str(exp)], capture_output=True).returncode, 1)


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
        (self.repo / "src" / "main.rs").write_text("fn main() {}\n")
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
        return {"src/lib.rs": b"pub fn f() {}\n", "src/main.rs": b"fn main() {}\n", "Cargo.toml.orig": (self.repo / "Cargo.toml").read_bytes(),
                "Cargo.toml": b'# normalized by cargo\n[package]\nversion = "0.6.0"\nname = "kasane-pdf"\n',
                "Cargo.lock": b"# generated\n"}

    def test_9_the_built_manifest_differs_from_the_tag(self):
        for name, toml in (
                ("a default feature", b'[package]\nname = "kasane-pdf"\nversion = "0.6.0"\n[features]\ndefault = ["x"]\nx = []\n'),
                ("a dependency", b'[package]\nname = "kasane-pdf"\nversion = "0.6.0"\n[dependencies]\nevil = "1"\n'),
                ("another version", b'[package]\nname = "kasane-pdf"\nversion = "0.7.0"\n')):
            with self.subTest(name):
                self.assertIn("manifest_differs_from_tag", self.check(self.crate(dict(self.good(), **{"Cargo.toml": toml}))))
        g = self.good()
        g.pop("Cargo.toml.orig")
        self.assertIn("manifest_missing", self.check(self.crate(g)))
        base = b'[package]\nname = "kasane-pdf"\nversion = "0.6.0"\n'
        for name, extra in (("the CLI redirected", b'[[bin]]\nname = "kasane-pdf"\npath = "src/alternate.rs"\n'),
                            ("another edition", None),
                            ("a target-only dependency", b'[target."cfg(unix)".dependencies]\nevil = "1"\n')):
            with self.subTest(name):
                toml = base.replace(b'version = "0.6.0"', b'version = "0.6.0"\nedition = "2024"') if extra is None \
                    else base + extra
                files = dict(self.good(), **{"Cargo.toml": toml, "src/alternate.rs": b"fn main() {}\n"})
                self.assertIn("manifest_differs_from_tag", self.check(self.crate(files)))
        self.assertIn("cargo_config_in_package",
                      self.check(self.crate(dict(self.good(), **{".cargo/config.toml": b"[build]\nrustc = 'x'\n"}))))

    def test_8_an_ancestor_cargo_config(self):
        d = Path(tempfile.mkdtemp())
        (d / "src" / "v").mkdir(parents=True)
        self.assertIsNone(ah.cargo_config_problem(d / "src" / "v")) if not ah.cargo_config_problem() else None
        (d / ".cargo").mkdir()
        (d / ".cargo" / "config.toml").write_text("[build]\nrustc-wrapper = 'x'\n")
        self.assertTrue(ah.cargo_config_problem(d / "src" / "v"))

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

    def test_11_cache_reuse_needs_the_same_provenance(self):
        cache = Path(tempfile.mkdtemp())
        (cache / "bin" / "0.6.0").mkdir(parents=True)
        (cache / "records").mkdir()
        b = cache / "bin" / "0.6.0" / "kasane-pdf"
        b.write_bytes(b"binary")
        cfg = {"toolchain": "1.92", "features": []}
        saved = ah.fetch_crate, ah.compiler, ah.cargo_config_problem
        fields = {"commit": "b" * 40, "crate_sha256": "a" * 64}
        ah.fetch_crate = lambda v, index, cache: ({"Cargo.toml": b"not a manifest"}, dict(fields), None)
        ah.compiler = lambda cfg: "rustc 1.92.0 (ded5c06cf 2025-12-08)\ncommit-hash: " + "c" * 40
        ah.cargo_config_problem = lambda src=None: None
        try:
            import platform as _p
            rec = {"status": "built", "version": "0.6.0", "tag": "v0.6.0", "source": "crates.io", "toolchain": "1.92",
                   "features": [], "platform": f"{_p.system()}-{_p.machine()}", "builder": ah.BUILDER,
                   "rustc": "rustc 1.92.0 (ded5c06cf 2025-12-08)", "rustc_commit": "c" * 40, **fields,
                   "binary_sha256": hashlib.sha256(b"binary").hexdigest()}
            (cache / "records" / "0.6.0.json").write_text(json.dumps(rec))
            self.assertEqual(ah.build_version("0.6.0", cfg, cache, cache / "log", index=[])["status"], "built")
            b.write_bytes(b"other binary")
            got = ah.build_version("0.6.0", cfg, cache, cache / "log", index=[])
            self.assertEqual((got["status"], got.get("problem")), ("error", "binary_digest"))
            # A record of another package, platform or builder is not reused:
            # the version is built again (here the fake package does not build).
            b.write_bytes(b"binary")
            for k, v in (("crate_sha256", "0" * 64), ("commit", "0" * 40), ("platform", "WrongOS-x"), ("builder", 1)):
                with self.subTest(k):
                    (cache / "records" / "0.6.0.json").write_text(json.dumps(dict(rec, **{k: v})))
                    got = ah.build_version("0.6.0", cfg, cache, cache / "log", index=[])
                    self.assertEqual((got["status"], got.get("problem")), ("error", "build_failed"))
            ah.cargo_config_problem = lambda src=None: "cargo_config:x"
            self.assertEqual(ah.build_version("0.6.0", cfg, cache, cache / "log", index=[]).get("problem"), "cargo_config")
        finally:
            ah.fetch_crate, ah.compiler, ah.cargo_config_problem = saved

    def test_8_the_build_environment_is_clean(self):
        os.environ.update(RUSTC="/evil/rustc", RUSTC_WRAPPER="/evil/wrap", CARGO_ENCODED_RUSTFLAGS="--cfg\x1ffeature")
        try:
            env = ah.build_env(Path("/c"), "0.6.0")
        finally:
            for k in ("RUSTC", "RUSTC_WRAPPER", "CARGO_ENCODED_RUSTFLAGS"):
                os.environ.pop(k)
        for k in ("RUSTC", "RUSTC_WRAPPER", "CARGO_ENCODED_RUSTFLAGS", "RUSTFLAGS"):
            self.assertNotIn(k, env)


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
        r = subprocess.run([sys.executable, str(ROOT / "scripts" / "advisory-history.py"), "run", "--mode", "main",
                            "--dump", "/x",
                            "--candidate", "/x", "--json", "/tmp/x.json", "--private-inputs", "/y",
                            "--private-json", "/tmp/y.json"], capture_output=True, text=True, env=env)
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("never run in GitHub Actions", r.stdout + r.stderr)



class Modes(unittest.TestCase):
    """`run --mode main|release-tag`, with the builds, the crates.io index,
    the renderers and the judging replaced: what each mode requires."""

    INDEX = ["0.5.0", "0.6.0", "0.7.0", "0.8.0"]

    def setUp(self):
        import platform as pf
        self.d = Path(tempfile.mkdtemp())
        self.dump = self.d / "dump"
        self.dump.mkdir()
        (self.dump / "manifest-a.tsv").write_text(f"a.pdf\t{GHSA}\tredact --page 1 --rect 1 1 2 2\tsafe-bytes:X\n")
        self.cand = self.d / "cand"
        self.cand.write_text("#!/bin/sh\n")
        self.cfg = {"versions": ["0.5.0", "0.6.0", "0.7.0"], "advisories": dict(ADV), "toolchain": "1.92",
                    "features": [], "expected": {"a.pdf": {"0.5.0": "NA", "0.6.0": "LEAK", "0.7.0": "LEAK",
                                                           "candidate": "REFUSED"}}}
        self.cfg["advisories"][GHSA] = {"affected": [">= 0.6.0", "< 0.8.0"], "patched": "0.8.0"}
        self.plat = f"{pf.system()}-{pf.machine()}"
        self.release_state = "REFUSED"
        self.saved = {k: getattr(ah, k) for k in ("build_version", "evaluate", "fetch", "load", "cache_dir")}

        def rec(v):
            return {"status": "built", "version": v, "tag": f"v{v}", "source": "crates.io", "toolchain": "1.92",
                    "features": [], "builder": ah.BUILDER, "platform": self.plat, "crate_sha256": "c",
                    "commit": "d", "rustc_commit": "e"}
        ah.build_version = lambda v, *a, **k: rec(v)
        # Published in the order of INDEX, a day apart, unless TIMES says.
        self.TIMES = None

        def fetch(url):
            if url.startswith(ah.VERSIONS_API):
                times = self.TIMES or {v: f"2026-01-{i + 1:02d}T00:00:00.000000Z" for i, v in enumerate(self.INDEX)}
                return json.dumps({"versions": [{"num": v, "created_at": t} for v, t in times.items()],
                                   "meta": {"next_page": None}}).encode()
            return "\n".join(json.dumps({"vers": v}) for v in self.INDEX).encode()
        ah.fetch = fetch
        ah.cache_dir = lambda a: self.d / "cache"
        self.saved_usage = ah.car.usage_of
        ah.car.usage_of = lambda path: "usage: kasane-pdf redact"

        class VR:
            @staticmethod
            def tool_versions():
                return {"poppler": "1", "mupdf": "1", "pdfjs": "1"}
        ah.load = lambda *a: VR

        def evaluate(rows, dump, binaries, usage, vr, jobs):
            states = {"0.5.0": "NA", "0.6.0": "LEAK", "0.7.0": "LEAK", "candidate": "REFUSED",
                      "release": self.release_state}
            return {r[0]: {label: {"state": states.get(label, "LEAK")} for label in binaries} for r in rows}
        ah.evaluate = evaluate

    def tearDown(self):
        for k, v in self.saved.items():
            setattr(ah, k, v)
        ah.car.usage_of = self.saved_usage

    def run_mode(self, *mode):
        exp = self.d / "history.json"
        exp.write_text(json.dumps(self.cfg))
        out = self.d / "out.json"
        with contextlib.redirect_stdout(io.StringIO()):
            code = ah.main(["run", *mode, "--dump", str(self.dump), "--candidate", str(self.cand), "--json", str(out),
                            "--expected", str(exp), "--log", str(self.d / "log")])
        return code, json.loads(out.read_text())

    def test_16_19_a_release_tag(self):
        # The tag 0.8.0, published; 0.9.0 came after it.
        self.INDEX = self.INDEX + ["0.9.0"]
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertEqual((code, doc["problems"]), (0, []))
        self.assertEqual((doc["mode"], doc["tag_version"], doc["after_tag"]), ("release-tag", "0.8.0", ["0.9.0"]))
        self.assertEqual(doc["tag_release"]["published"], True)
        self.assertEqual(doc["tag_release"]["differs"], 0)
        # Not judged is not safe: it is in no range, no record, no cell.
        self.assertNotIn("0.9.0", doc["versions"])
        self.assertNotIn("0.9.0", doc["inputs"]["a.pdf"]["cells"])
        # The same data in main mode: 0.8.0 and 0.9.0 are not covered.
        code, doc = self.run_mode("--mode", "main")
        self.assertEqual(code, 1)
        self.assertIn("version_not_covered:0.8.0", doc["problems"])
        self.assertIn("version_not_covered:0.9.0", doc["problems"])

    def test_16_a_tag_not_published(self):
        # Nothing to check the tag's build against: a failure, even with a
        # history that covers every version before it.
        self.cfg["versions"].append("0.8.0")
        self.cfg["expected"]["a.pdf"]["0.8.0"] = "LEAK"
        self.cfg["advisories"][GHSA]["affected"] = [">= 0.6.0", "< 0.9.0"]
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.9.0")
        self.assertEqual(code, 1)
        self.assertIn("tag_not_published:0.9.0", doc["problems"])
        self.assertEqual(doc["tag_release"], {"version": "0.9.0", "published": False})
        # Yanked is not published either.
        self.INDEX = ["0.5.0", "0.6.0", "0.7.0"]
        self.saved_fetch = ah.fetch
        ah.fetch = lambda url: (self.saved_fetch(url) if url.startswith(ah.VERSIONS_API) else
                                "\n".join(json.dumps({"vers": v, "yanked": v == "0.8.0"})
                                          for v in self.INDEX + ["0.8.0"]).encode())
        self.cfg["versions"].remove("0.8.0")
        del self.cfg["expected"]["a.pdf"]["0.8.0"]
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertIn("tag_not_published:0.8.0", doc["problems"])

    def test_19_after_is_by_publication_time(self):
        # 0.6.1, a patch of the 0.6 line, published after the 0.8.0 tag: not
        # in that tag's history and not required by it — and not judged.
        self.INDEX = self.INDEX + ["0.6.1"]
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertEqual((code, doc["problems"]), (0, []))
        self.assertEqual(doc["after_tag"], ["0.6.1"])
        self.assertNotIn("0.6.1", doc["inputs"]["a.pdf"]["cells"])
        # main still requires it.
        code, doc = self.run_mode("--mode", "main")
        self.assertIn("version_not_covered:0.6.1", doc["problems"])
        # A higher version published before the tag (the tag is the
        # backport 0.6.1): required by it.
        self.INDEX = ["0.5.0", "0.6.0", "0.7.0", "0.8.0", "0.6.1"]
        self.cfg["advisories"][GHSA] = {"affected": [">= 0.6.0", "< 0.6.1"], "patched": "0.6.1"}
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.6.1")
        self.assertIn("version_not_covered:0.8.0", doc["problems"])
        # A published version without a publication time: a failure.
        self.TIMES = {"0.5.0": "2026-01-01T00:00:00Z", "0.6.0": "2026-01-02T00:00:00Z",
                      "0.7.0": "2026-01-03T00:00:00Z", "0.8.0": "2026-01-04T00:00:00Z"}
        self.INDEX = ["0.5.0", "0.6.0", "0.7.0", "0.8.0", "0.9.0"]
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertIn("no_publication_time:0.9.0", doc["problems"])
        self.TIMES = dict(self.TIMES, **{"0.9.0": "yesterday"})
        with self.assertRaises(SystemExit):
            self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        # Published at the tag's own time: neither before nor after.
        self.TIMES = dict(self.TIMES, **{"0.9.0": "2026-01-04T00:00:00Z"})
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertEqual(code, 1)
        self.assertIn("publication_time_of_tag:0.9.0", doc["problems"])
        self.assertEqual(doc["after_tag"], [])
        # A version listed twice by the API.
        saved = ah.fetch
        ah.fetch = lambda url: (json.dumps({"versions": [{"num": "0.8.0", "created_at": "2026-01-04T00:00:00Z"},
                                                         {"num": "0.8.0", "created_at": "2026-01-09T00:00:00Z"}],
                                            "meta": {}}).encode() if url.startswith(ah.VERSIONS_API) else saved(url))
        with self.assertRaises(SystemExit):
            self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        ah.fetch = saved

    def test_17_a_version_before_the_tag_missing(self):
        self.cfg["versions"].remove("0.6.0")
        del self.cfg["expected"]["a.pdf"]["0.6.0"]
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertEqual(code, 1)
        self.assertIn("version_not_covered:0.6.0", doc["problems"])

    def test_18_the_tag_itself(self):
        # The published release is not what the tag builds.
        self.release_state = "LEAK"
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertEqual(code, 1)
        self.assertIn("tag_release_differs:a.pdf", doc["problems"])
        # The tag's history naming the tag or a later version.
        self.release_state = "REFUSED"
        self.cfg["versions"].append("0.8.0")
        self.cfg["expected"]["a.pdf"]["0.8.0"] = "REFUSED"
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertEqual(code, 1)
        self.assertIn("history_version_not_before_tag:0.8.0", doc["problems"])

    def test_20_21_25_the_mode_is_never_guessed(self):
        for argv in (["--mode", "release-tag"], ["--mode", "release-tag", "--tag-version", "v0.8.0"],
                     ["--mode", "release-tag", "--tag-version", "0.8"], ["--mode", "main", "--tag-version", "0.8.0"]):
            with self.assertRaises(SystemExit, msg=argv):
                self.run_mode(*argv)
        with contextlib.redirect_stderr(io.StringIO()):
            for argv in ([], ["--mode", "tag"], ["--mode", "nightly"]):
                with self.assertRaises(SystemExit, msg=argv):
                    self.run_mode(*argv)

    def test_22_23_nothing_to_judge(self):
        for f in self.dump.iterdir():
            f.unlink()
        code, doc = self.run_mode("--mode", "release-tag", "--tag-version", "0.8.0")
        self.assertEqual(code, 1)
        self.assertIn("no_inputs", doc["problems"])

    def test_22_no_versions(self):
        self.cfg["versions"] = []
        self.cfg["expected"]["a.pdf"] = {"candidate": "REFUSED"}
        self.INDEX = []
        code, doc = self.run_mode("--mode", "main")
        self.assertEqual(code, 1)
        self.assertIn("no_versions", doc["problems"])

    def test_build_reports(self):
        exp = self.d / "history.json"
        exp.write_text(json.dumps(self.cfg))
        with contextlib.redirect_stdout(io.StringIO()) as out:
            self.assertEqual(ah.main(["build", "--expected", str(exp)]), 1)  # 0.8.0 not covered
        self.assertIn("problem: version_not_covered:0.8.0", out.getvalue())
        self.INDEX = ["0.5.0", "0.6.0", "0.7.0"]
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(ah.main(["build", "--expected", str(exp)]), 0)

    def test_24_a_range_it_cannot_read(self):
        for bad in (["~0.6"], [], [">= 0.6"], "<= 0.8.0", [">= 0.6.0", 7]):
            self.cfg["advisories"][GHSA]["affected"] = bad
            with self.assertRaises(SystemExit, msg=bad):
                self.run_mode("--mode", "main")
        self.cfg["advisories"][GHSA] = {"affected": [">= 0.6.0"], "patched": "next"}
        with self.assertRaises(SystemExit):
            self.run_mode("--mode", "main")


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
        return {"mode": "main", "problems": list(problems),
                "inputs": {"a.pdf": {"cells": {k.replace("_", "."): {"state": v} for k, v in states.items()}}}}

    def test_only_a_main_run_updates(self):
        for mode in ("release-tag", None):
            res = dict(self.results(**norm(GOOD)), mode=mode)
            code, doc, out = self.run_update(res, {"expected": {}})
            self.assertEqual((code, doc["expected"]), (1, {}), mode)

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

    def test_5_an_unexpected_refusal_cannot_be_baselined(self):
        results = {"syn-xfa": {"outcome": "refused", "expected": "ok", "renderers": {}},
                   "adv:new": {"outcome": "ok", "expected": "unknown", "renderers": {
                       r: {"changed": 0} for r in vrend.RENDERERS}}}
        problems, _ = vrend.judge(results, {"poppler": "1", "mupdf": "1", "pdfjs": "1"}, None)
        self.assertIn("outcome_not_expected:syn-xfa", problems)
        self.assertIn("no_outcome:adv:new", problems)
        self.assertEqual(len(vrend.safety_problems(problems)), 2)
        self.assertEqual(vrend.SYNTHETIC_OUTCOME["syn-xfa"], "ok")
        self.assertTrue(all(v for v in vrend.history_outcomes().values()))

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
