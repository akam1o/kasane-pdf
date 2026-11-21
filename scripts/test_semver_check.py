#!/usr/bin/env python3
"""Tests of scripts/semver-check.py: detection is never skipped, the
judgement follows the version change, a check that did not run is never
a pass.

    python3 scripts/test_semver_check.py

The parsing and judgement tests need nothing; the end-to-end ones build
small synthetic crates and need cargo-semver-checks (skipped without it;
KASANE_REQUIRE_SEMVER_CHECKS=1 makes its absence a failure, as in the
full gate's semver job).
"""
import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import textwrap
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("semver_check", os.path.join(HERE, "semver-check.py"))
sc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(sc)

HAVE_TOOL = shutil.which("cargo") is not None and subprocess.run(
    ["cargo", "semver-checks", "--version"], capture_output=True).returncode == 0
if not HAVE_TOOL and os.environ.get("KASANE_REQUIRE_SEMVER_CHECKS") == "1":
    raise SystemExit("cargo-semver-checks is required (KASANE_REQUIRE_SEMVER_CHECKS=1)")

REPORT = """\
    Checking kasane-pdf v0.1.0 -> v0.2.0 (assume patch change)
    Starting 2 checks, 1 unnecessary on 10 threads
     Checked [   0.007s] 229 checks: 228 pass, 1 fail, 0 warn, 31 skip

--- failure function_missing: pub fn removed or renamed ---

Description:
A publicly-visible function cannot be imported by its prior path.

Failed in:
  function kasane_pdf::removed, previously in file /tmp/x/src/lib.rs:2

     Summary semver requires new major version: 1 major and 0 minor checks failed
"""
LEVELS = {"function_missing": "major", "function_marked_deprecated": "minor", "pub_const_added": "minor"}
RAN = "".join(f"        {r} [   0.000s]       {LEVELS[i]}        {i}\n"
              for r, i in (("FAIL", "function_missing"), ("PASS", "function_marked_deprecated")))
REPORT = REPORT.replace("229 checks: 228 pass, 1 fail, 0 warn, 31 skip", "2 checks: 1 pass, 1 fail, 0 warn, 1 skip") + RAN


class Versions(unittest.TestCase):
    def test_bump(self):
        b = sc.bump
        self.assertEqual(b("0.11.0", "0.11.0"), "same")
        self.assertEqual(b("0.11.0", "0.11.1"), "compatible")
        self.assertEqual(b("0.11.0", "0.12.0"), "incompatible")
        self.assertEqual(b("0.0.3", "0.0.4"), "incompatible")
        self.assertEqual(b("1.2.3", "1.3.0"), "compatible")
        self.assertEqual(b("1.2.3", "2.0.0"), "incompatible")
        self.assertEqual(b("0.12.0", "0.11.0"), "downgrade")
        self.assertIsNone(b("0.12.0", "0.12.0-rc.1"))


class Judgement(unittest.TestCase):
    SECTION = {"0.2.0": "0.2.0\n\n- `function_missing`: `kasane_pdf::removed` is gone.\n"}

    def judge(self, text=REPORT, base="0.1.0", cand="0.2.0", sections=None):
        return sc.judge(sc.parse_output(text), LEVELS, base, cand, self.SECTION if sections is None else sections)

    def test_parse(self):
        p = sc.parse_output(REPORT)
        self.assertEqual(p["checking"], ("kasane-pdf", "0.1.0", "0.2.0", "assume patch change"))
        self.assertEqual(p["starting"], (2, 1))
        self.assertEqual(p["checks"], {"total": 2, "pass": 1, "fail": 1, "warn": 0, "skip": 1})
        self.assertEqual([(f["id"], f["items"]) for f in p["findings"]], [("function_missing", ["function kasane_pdf::removed"])])

    def test_breaking_needs_an_incompatible_version_and_its_section(self):
        not_run, judged, refused = self.judge()
        self.assertEqual((not_run, refused), ([], []))
        self.assertTrue(judged[0]["allowed"])
        self.assertIn("api-stability", self.judge(sections={})[2][0])
        # A section that does not name the lint, or the item: refused.
        for text in ("0.2.0\n\nSomething else changed.\n", "0.2.0\n\n`function_missing` somewhere.\n"):
            self.assertTrue(self.judge(sections={"0.2.0": text})[2], text)
        for cand in ("0.1.1", "0.1.0"):
            not_run, _, refused = self.judge(cand=cand, text=REPORT.replace("v0.2.0", f"v{cand}"),
                                             sections={cand: self.SECTION["0.2.0"]})
            self.assertEqual(not_run, [])
            self.assertTrue(refused and "incompatible version change" in refused[0], refused)

    def test_an_addition_is_allowed_and_listed(self):
        text = (REPORT.replace("function_missing: pub fn removed or renamed", "function_marked_deprecated: fn deprecated")
                .replace("FAIL [   0.000s]       major        function_missing", "PASS [   0.000s]       major        function_missing")
                .replace("PASS [   0.000s]       minor        function_marked_deprecated", "FAIL [   0.000s]       minor        function_marked_deprecated"))
        not_run, judged, refused = self.judge(text=text.replace("v0.2.0", "v0.1.1"), cand="0.1.1")
        self.assertEqual((not_run, refused), ([], []))
        self.assertEqual((judged[0]["level"], judged[0]["allowed"]), ("minor", True))

    def test_not_run_is_never_a_pass(self):
        cases = {
            "no checking line": REPORT.split("\n", 1)[1],
            "not every lint": REPORT.replace("assume patch change", "major change"),
            "other versions": REPORT.replace("v0.1.0 -> v0.2.0", "v0.2.0 -> v0.2.0"),
            "zero checks": REPORT.replace("2 checks: 1 pass, 1 fail", "0 checks: 0 pass, 0 fail"),
            "no checked line": "\n".join(l for l in REPORT.splitlines() if "Checked" not in l),
            "counts": REPORT.replace("1 pass, 1 fail", "0 pass, 1 fail"),
            "a breaking lint did not run": REPORT.replace("FAIL [   0.000s]       major        function_missing\n", ""),
            "undescribed failure": REPORT.replace("--- failure function_missing", "--- note function_missing"),
            "unknown lint": REPORT.replace("function_missing", "lint_from_the_future"),
            "another crate": REPORT.replace("Checking kasane-pdf", "Checking synth"),
            "undescribed warning": REPORT.replace("1 pass, 1 fail, 0 warn", "0 pass, 1 fail, 1 warn").replace(
                "PASS [   0.000s]       minor        function_marked_deprecated",
                "WARN [   0.000s]       minor        function_marked_deprecated"),
            "unknown passing lint": REPORT + "        PASS [   0.000s]       major        lint_from_the_future\n",
            "mislevelled": REPORT.replace("minor        function_marked_deprecated", "major        function_marked_deprecated"),
            "listed twice": REPORT + "        PASS [   0.000s]       minor        function_marked_deprecated\n",
            "summary": REPORT.replace("semver requires new major version", "no semver update required"),
            "not the unnecessary ones": REPORT.replace("1 unnecessary", "0 unnecessary"),
            "no starting line": REPORT.replace("    Starting 2 checks, 1 unnecessary on 10 threads\n", ""),
        }
        for name, text in cases.items():
            not_run, _, _ = self.judge(text=text)
            self.assertTrue(not_run, name)


FAKE = r"""#!/usr/bin/env python3
import sys
REPORT = %r
args = sys.argv[1:]
if args[:1] == ["--version"]:
    print("cargo-semver-checks 0.51.0 (fake)")
elif args[:1] == ["--list"]:
    print("id type description\n== ==== ===")
    for i, l in %r.items():
        print(i, l, "x")
else:
    out, err = REPORT.split("\n--- ", 1) if "\n--- " in REPORT else (REPORT, "")
    sys.stderr.write(out)
    sys.stdout.write(("--- " + err) if err else "")
    sys.exit(%d)
"""


@unittest.skipUnless(shutil.which("cargo"), "cargo not installed")
class ExitStatus(unittest.TestCase):
    """Only 0 without failing checks and 100 with a full report count;
    another status after a plausible report did not run."""

    def run_fake(self, status, report=REPORT):
        with tempfile.TemporaryDirectory() as d:
            tool = os.path.join(d, "tool")
            with open(tool, "w") as fh:
                fh.write(FAKE % (report, LEVELS, status))
            os.chmod(tool, 0o755)
            pkgs = {}
            for name, v in (("base", "0.1.0"), ("cand", "0.2.0")):
                root = os.path.join(d, name)
                EndToEnd.write(root, v, "pub fn kept() {}\n", sections={"0.2.0": Judgement.SECTION["0.2.0"]})
                pkgs[name] = root
            crate = os.path.join(d, "kasane-pdf-0.1.0.crate")
            with sc.tarfile.open(crate, "w:gz") as t:
                t.add(pkgs["base"], arcname="kasane-pdf-0.1.0")
            sha = sc.hashlib.sha256(open(crate, "rb").read()).hexdigest()
            out = os.path.join(d, "o.json")
            rc = sc.main(["--manifest-dir", pkgs["cand"], "--baseline-crate", crate, "--baseline-sha256", sha,
                          "--tool", tool, "--json", out])
            return rc, sc.json.load(open(out))

    def test_status(self):
        rc, doc = self.run_fake(100)
        self.assertEqual(rc, 0, doc)  # a full report, the finding allowed by 0.1 -> 0.2 and its section
        for status in (101, 1, 2):
            rc, doc = self.run_fake(status)
            self.assertEqual(rc, 2, (status, doc))
        rc, doc = self.run_fake(0)  # failing checks with exit 0
        self.assertEqual(rc, 2, doc)


@unittest.skipUnless(HAVE_TOOL, "cargo-semver-checks not installed (KASANE_REQUIRE_SEMVER_CHECKS=1 makes this a failure)")
class EndToEnd(unittest.TestCase):
    BASE = "pub fn kept() -> u32 { 1 }\npub fn removed() -> u32 { 2 }\n"

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.mkdtemp(prefix="semver-test-")
        cls.crate, cls.sha = cls.package(os.path.join(cls.tmp, "base"), "0.1.0", cls.BASE)

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.tmp, ignore_errors=True)

    @staticmethod
    def write(root, version, lib, sections=None):
        os.makedirs(os.path.join(root, "src"), exist_ok=True)
        os.makedirs(os.path.join(root, "docs"), exist_ok=True)
        with open(os.path.join(root, "Cargo.toml"), "w") as fh:
            fh.write(textwrap.dedent(f"""\
                [package]
                name = "kasane-pdf"
                version = "{version}"
                edition = "2021"
                description = "synthetic"
                license = "Apache-2.0"

                [workspace]
                """))
        with open(os.path.join(root, "src", "lib.rs"), "w") as fh:
            fh.write(lib)
        with open(os.path.join(root, "docs", "api-stability.md"), "w") as fh:
            fh.write("# API\n\n" + "".join(f"## {v}\n\n{t}\n" for v, t in (sections or {}).items()))

    @classmethod
    def package(cls, root, version, lib):
        cls.write(root, version, lib)
        subprocess.run(["cargo", "package", "-q", "--allow-dirty", "--no-verify"], cwd=root, check=True)
        src = os.path.join(root, "target", "package", f"kasane-pdf-{version}.crate")
        dst = os.path.join(cls.tmp, f"published-{version}.crate")
        shutil.copy(src, dst)
        return dst, sc.hashlib.sha256(open(dst, "rb").read()).hexdigest()

    def check(self, version, lib, sections=None, crate=None, sha=None, name="cand"):
        root = os.path.join(self.tmp, name)
        shutil.rmtree(root, ignore_errors=True)
        self.write(root, version, lib, sections)
        out = os.path.join(self.tmp, name + ".json")
        rc = sc.main(["--manifest-dir", root, "--baseline-crate", crate or self.crate,
                      "--baseline-sha256", sha or self.sha, "--json", out])
        return rc, sc.json.load(open(out)), root

    def test_unchanged_passes_with_every_lint_run(self):
        rc, doc, _ = self.check("0.1.0", self.BASE)
        self.assertEqual(rc, 0, doc)
        self.assertGreater(doc["checks"]["total"], 0)
        self.assertEqual((doc["findings"], doc["mode"]), ([], "assume patch change"))
        # Only the tool's "unnecessary" additions are left out, by name.
        levels = sc.lint_levels(["cargo", "semver-checks"])
        self.assertTrue(doc["skipped_lints"])
        self.assertEqual({levels[l] for l in doc["skipped_lints"]}, {"minor"})

    def test_a_minor_bump_still_detects(self):
        lib = "pub fn kept() -> u32 { 1 }\n"
        rc, doc, _ = self.check("0.2.0", lib)
        self.assertEqual(rc, 1, doc)  # detected; no api-stability section
        self.assertEqual([f["id"] for f in doc["findings"]], ["function_missing"])
        rc, doc, _ = self.check("0.2.0", lib, sections={"0.2.0": "Removed."})
        self.assertEqual(rc, 1, doc)  # a section that names neither the lint nor the item
        named = "`function_missing`: `kasane_pdf::removed` removed."
        rc, doc, _ = self.check("0.2.0", lib, sections={"0.2.0": named})
        self.assertEqual(rc, 0, doc)  # detected and allowed, listed
        self.assertEqual([(f["id"], f["allowed"]) for f in doc["findings"]], [("function_missing", True)])
        for v in ("0.1.1", "0.1.0"):
            rc, doc, _ = self.check(v, lib, sections={v: named})
            self.assertEqual(rc, 1, (v, doc))

    def test_an_addition_in_a_patch(self):
        lib = "#[deprecated]\npub fn kept() -> u32 { 1 }\npub fn removed() -> u32 { 2 }\n"
        rc, doc, _ = self.check("0.1.1", lib)
        self.assertEqual(rc, 0, doc)
        self.assertEqual([(f["id"], f["level"], f["allowed"]) for f in doc["findings"]],
                         [("function_marked_deprecated", "minor", True)])

    def test_the_baseline_is_never_the_checkout(self):
        root = os.path.join(self.tmp, "self")
        self.write(root, "0.1.0", self.BASE)
        subprocess.run(["cargo", "package", "-q", "--allow-dirty", "--no-verify"], cwd=root, check=True)
        own = os.path.join(root, "target", "package", "kasane-pdf-0.1.0.crate")
        sha = sc.hashlib.sha256(open(own, "rb").read()).hexdigest()
        out = os.path.join(self.tmp, "self.json")
        rc = sc.main(["--manifest-dir", root, "--baseline-crate", own, "--baseline-sha256", sha, "--json", out])
        self.assertEqual(rc, 2)
        self.assertIn("self-comparison", " ".join(sc.json.load(open(out))["problems_not_run"]))

    def test_not_run(self):
        rc, doc, _ = self.check("0.1.0", self.BASE, sha="0" * 64, name="badsha")
        self.assertEqual(rc, 2, doc)
        rc, doc, _ = self.check("0.1.0", "pub fn kept( -> u32 {\n", name="broken")
        self.assertEqual(rc, 2, doc)
        self.assertEqual(doc["status"], "not_run")


if __name__ == "__main__":
    unittest.main()
