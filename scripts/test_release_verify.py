#!/usr/bin/env python3
"""Tests of scripts/release-verify.py and scripts/verify-renderers.py
(pr.yml compat-smoke). Each test makes a throwaway git repository with a
copy of the verifier and a configuration of plain commands, runs the
real `run`, then tampers with the results the way a mix-up or a forgery
would and checks that `check` refuses them — and that a clean run
passes. No corpus, renderer or cargo is needed.

    python3 scripts/test_release_verify.py
"""
import datetime
import hashlib
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
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
VERIFIER = ROOT / "scripts" / "release-verify.py"


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / file)
    mod = importlib.util.module_from_spec(spec)
    argv, sys.argv = sys.argv, [sys.argv[0]]
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.argv = argv
    return mod


rv = load("release_verify", "scripts/release-verify.py")
vrend = load("verify_renderers", "scripts/verify-renderers.py")
PLANTED = "Quarterly figures for ACME at 61.5 712.25 /Users/someone/private.pdf AKIAFAKEKEY"


def command(code="pass", timeout=30, tools=()):
    return {"kind": "command", "timeout": timeout, "tools": list(tools),
            "argv": [sys.executable, "-c", code]}


class Repo:
    """A git repository holding the verifier and a configuration."""

    def __init__(self, checks, profile=("a", "b"), name="test"):
        self.tmp = Path(tempfile.mkdtemp(prefix="rv-test-"))
        self.path = self.tmp / "repo"
        self.path.mkdir()
        self.ledger = self.tmp / "ledger.jsonl"
        self.git("init", "-q", "-b", "main")
        self.git("config", "user.email", "t@example.invalid")
        self.git("config", "user.name", "t")
        (self.path / "scripts").mkdir()
        shutil.copyfile(VERIFIER, self.path / "scripts" / "release-verify.py")
        (self.path / "README").write_text("readme\n")
        self.commit(checks, profile, name=name)

    def git(self, *args):
        return subprocess.run(["git", "-C", str(self.path), *args], capture_output=True, text=True, check=True).stdout.strip()

    def commit(self, checks, profile=("a", "b"), message="c", name="test"):
        config = {"schema_version": 1, "max_age_days": 30, "profiles": {name: list(profile)}, "checks": checks}
        (self.path / "scripts" / "release-verify.json").write_text(json.dumps(config, indent=1))
        self.git("add", "-A")
        self.git("commit", "-qm", message, "--allow-empty")
        return self.git("rev-parse", "HEAD")

    def env(self):
        return dict(os.environ, KASANE_RELEASE_VERIFY_LEDGER=str(self.ledger))

    def run(self, ref="HEAD", only=None, out=None, profile="test"):
        out = out or self.tmp / f"out-{time.monotonic_ns()}"
        argv = [sys.executable, str(self.path / "scripts" / "release-verify.py"), "run", "--ref", ref, "--profile", profile,
                "--out", str(out), "--corpus", str(self.tmp / "corpus")]
        for o in only or []:
            argv += ["--only", o]
        r = subprocess.run(argv, cwd=self.path, capture_output=True, text=True, env=self.env())
        return r.returncode, out, r.stdout + r.stderr

    def verify(self, results, ref="HEAD", profile="test", **kw):
        old = os.environ.get("KASANE_RELEASE_VERIFY_LEDGER")
        os.environ["KASANE_RELEASE_VERIFY_LEDGER"] = str(self.ledger)
        try:
            return rv.verify(self.path, ref, results, profile, **kw)
        finally:
            if old is None:
                os.environ.pop("KASANE_RELEASE_VERIFY_LEDGER")
            else:
                os.environ["KASANE_RELEASE_VERIFY_LEDGER"] = old


def rewrite(out, name, edit):
    """Edit checks/<name>.json and record its new digest in result.json —
    a consistent forgery of that file."""
    p = out / "checks" / f"{name}.json"
    doc = json.loads(p.read_text())
    edit(doc)
    data = json.dumps(doc).encode()
    p.write_bytes(data)
    res = json.loads((out / "result.json").read_text())
    res["checks"][name]["sha256"] = hashlib.sha256(data).hexdigest()
    res["checks"][name]["status"] = doc["status"]
    (out / "result.json").write_text(json.dumps(res))


def edit_result(out, edit):
    res = json.loads((out / "result.json").read_text())
    edit(res)
    (out / "result.json").write_text(json.dumps(res))


class Passing(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.repo = Repo({"a": command(), "b": command()})
        code, cls.out, cls.log = cls.repo.run()
        assert code == 0, cls.log

    def fresh(self):
        """A copy of the passing results to tamper with."""
        d = Path(tempfile.mkdtemp()) / "copy"
        shutil.copytree(self.out, d, symlinks=True)
        return d

    def test_a_clean_run_passes(self):
        self.assertEqual(self.repo.verify(self.out), [])

    def test_1_another_commit(self):
        sha = self.repo.git("rev-parse", "HEAD")
        other = self.repo.commit({"a": command(), "b": command()}, message="other")
        try:
            self.assertTrue(any("results of commit" in p for p in self.repo.verify(self.out, ref=other)))
        finally:
            self.repo.git("reset", "-q", "--hard", sha)

    def test_2_another_tree(self):
        d = self.fresh()
        edit_result(d, lambda r: r.__setitem__("tree", "0" * 40))
        self.assertTrue(any("another tree" in p for p in self.repo.verify(d)))

    def test_4_edited_check(self):
        d = self.fresh()
        p = d / "checks" / "a.json"
        p.write_text(p.read_text().replace('"duration_seconds"', '"duration_seconds" ', 1))
        self.assertTrue(any("digest" in p for p in self.repo.verify(d)))

    def test_4_truncated_result(self):
        d = self.fresh()
        data = (d / "result.json").read_bytes()
        (d / "result.json").write_bytes(data[: len(data) // 2])
        self.assertTrue(any("not complete JSON" in p for p in self.repo.verify(d)))

    def test_5_missing_check(self):
        d = self.fresh()
        (d / "checks" / "b.json").unlink()
        self.assertTrue(any("missing" in p for p in self.repo.verify(d)))
        d = self.fresh()
        edit_result(d, lambda r: r["checks"].pop("b"))
        self.assertTrue(any("b: no result" in p for p in self.repo.verify(d)))

    def test_6_unknown_status(self):
        d = self.fresh()
        edit_result(d, lambda r: r.__setitem__("status", "PASSED"))
        self.assertTrue(any("unknown status" in p for p in self.repo.verify(d)))

    def test_7_missing_log(self):
        d = self.fresh()
        (d / "logs" / "a.log").unlink()
        self.assertTrue(any("missing: logs/a.log" in p for p in self.repo.verify(d)))

    def test_8_log_digest(self):
        d = self.fresh()
        with open(d / "logs" / "a.log", "a") as fh:
            fh.write("more\n")
        self.assertTrue(any("logs/a.log" in p and "digest" in p for p in self.repo.verify(d)))

    def test_config_and_verifier_mismatch(self):
        d = self.fresh()
        rewrite(d, "a", lambda doc: doc["config"].__setitem__("timeout", 1))
        self.assertTrue(any("another configuration" in p for p in self.repo.verify(d)))
        d = self.fresh()
        edit_result(d, lambda r: r["verifier"].__setitem__("blob", "1" * 40))
        self.assertTrue(any("another version of scripts/release-verify.py" in p for p in self.repo.verify(d)))
        d = self.fresh()
        edit_result(d, lambda r: r["config"].__setitem__("blob", "1" * 40))
        self.assertTrue(any("another scripts/release-verify.json" in p for p in self.repo.verify(d)))

    def test_check_from_another_run(self):
        d = self.fresh()
        rewrite(d, "a", lambda doc: doc.__setitem__("run_id", "0" * 32))
        self.assertTrue(any("another check or run" in p for p in self.repo.verify(d)))

    def test_too_old(self):
        later = datetime.datetime.now(datetime.timezone.utc) + datetime.timedelta(days=31)
        self.assertTrue(any("older than" in p for p in self.repo.verify(self.out, now=later)))

    def test_16_paths(self):
        d = self.fresh()
        edit_result(d, lambda r: r["checks"]["a"].__setitem__("file", "../../result.json"))
        self.assertTrue(self.repo.verify(d))
        d = self.fresh()
        rewrite(d, "a", lambda doc: doc["logs"].__setitem__("../../../etc/hosts", "0" * 64))
        self.assertTrue(any("a log outside the check" in p for p in self.repo.verify(d)))
        d = self.fresh()
        edit_result(d, lambda r: r["checks"].__setitem__("../x", {"status": "passed", "file": "x.json", "sha256": ""}))
        self.assertTrue(any("malformed entry" in p for p in self.repo.verify(d)))

    def test_17_symlinks(self):
        d = self.fresh()
        target = d / "checks" / "a.json"
        copy = d.parent / "elsewhere.json"
        shutil.copyfile(target, copy)
        target.unlink()
        target.symlink_to(copy)
        self.assertTrue(any("symbolic link" in p for p in self.repo.verify(d)))
        d = self.fresh()
        log = d / "logs" / "a.log"
        outside = d.parent / "outside.log"
        shutil.copyfile(log, outside)
        log.unlink()
        log.symlink_to(outside)
        self.assertTrue(any("symbolic link" in p for p in self.repo.verify(d)))
        d = self.fresh()
        link = d.parent / "link"
        link.symlink_to(d)
        self.assertTrue(any("symbolic link" in p for p in self.repo.verify(link)))
        d = self.fresh()
        (d / "checks").rename(d / "checks-real")
        (d / "checks").symlink_to(d / "checks-real")
        self.assertTrue(any("symbolic link" in p for p in self.repo.verify(d)))

    def test_a_command_log_not_recorded(self):
        d = self.fresh()
        rewrite(d, "a", lambda doc: doc["commands"][0].__setitem__("log", "a-other.log"))
        self.assertTrue(any("log is not recorded" in p for p in self.repo.verify(d)))

    def test_fifo_and_oversized_files(self):
        d = self.fresh()
        (d / "result.json").unlink()
        os.mkfifo(d / "result.json")
        def blocked(*_):
            raise TimeoutError("reading a FIFO blocked")
        old_handler = signal.signal(signal.SIGALRM, blocked)
        signal.alarm(10)
        try:
            self.assertTrue(any("not a regular file" in p for p in self.repo.verify(d)))
        finally:
            signal.alarm(0)
            signal.signal(signal.SIGALRM, old_handler)
        old = rv.MAX_JSON
        rv.MAX_JSON = 16
        try:
            self.assertTrue(any("too large" in p for p in self.repo.verify(self.out)))
        finally:
            rv.MAX_JSON = old

    def test_malformed_types(self):
        d = self.fresh()
        edit_result(d, lambda r: r.__setitem__("checks", {"a": ["not", "a", "dict"]}))
        self.assertTrue(self.repo.verify(d))
        d = self.fresh()
        rewrite(d, "a", lambda doc: doc.__setitem__("commands", "x"))
        self.assertTrue(self.repo.verify(d))


class Public(unittest.TestCase):
    def test_15_public_projection_keeps_no_private_text(self):
        repo = Repo({"security_regressions": command(), "redact_corpus": command(), "fuzz": command()},
                    ("security_regressions", "redact_corpus", "fuzz"))
        code, d, log = repo.run()
        self.assertEqual(code, 0, log)
        secret = "private_customer_name"
        rewrite(d, "security_regressions", lambda doc: (
            doc.update(kind="security_regressions", reason=secret + ":x"),
            doc["summary"].update({"verify_redact_ok": 35, "note": PLANTED, secret: 3, "verify_redact_cases": 61.5,
                                   "advisory_cases": True}),
            doc["commands"].append({"argv": [PLANTED], "exit": 0, "log": "security_regressions.log"})))
        rewrite(d, "redact_corpus", lambda doc: (
            doc.update(kind="redact_corpus", reason=PLANTED),
            doc["summary"].update({"configs": {
                secret: {"rect": {"x": 61.5, "y": 712.25}},
                "default": {"runs": 117, "status": "passed", secret: 5, "rect": [61.5, 712.25],
                            "lost": {"outside_words_lost": 2, secret: 61, "x": {"y": 712}}}}})))
        rewrite(d, "fuzz", lambda doc: (
            doc.update(kind="fuzz"),
            doc["summary"].update({"seconds": 900, "targets": {secret: {"runs": 1}, "redact_model": {
                "runs": 9, "binary_sha256": "ab" * 32, "path": PLANTED}}})))
        edit_result(d, lambda r: r["environment"]["tools"].update({"qpdf": PLANTED, "secret_env": "AKIAFAKEKEY",
                                                                     "poppler": "pdftoppm version 61.5"}))
        pub = repo.tmp / "public.json"
        subprocess.run([sys.executable, str(VERIFIER), "report", "--results", str(d), "--public", str(pub)],
                       capture_output=True, text=True)
        text = pub.read_text()
        for needle in ("Quarterly", "ACME", "61.5", "712", "/Users/", "private.pdf", "AKIAFAKEKEY", "rect", "argv",
                       secret, "note", "path", "binary_sha256", "secret_env"):
            self.assertNotIn(needle, text)
        doc = json.loads(text)
        self.assertEqual(doc["checks"]["security_regressions"]["summary"], {"verify_redact_ok": 35})
        self.assertEqual(doc["checks"]["security_regressions"]["reason"], "other")
        self.assertEqual(doc["checks"]["redact_corpus"]["summary"]["configs"],
                         {"default": {"runs": 117, "status": "passed", "lost": {"outside_words_lost": 2}}})
        self.assertEqual(doc["checks"]["fuzz"]["summary"]["targets"], {"redact_model": {"runs": 9}})
        self.assertNotIn("qpdf", doc["environment"]["tools"])


class Superseded(unittest.TestCase):
    def test_3_an_older_success_after_a_newer_run(self):
        repo = Repo({"a": command(), "b": command()})
        code, first, log = repo.run()
        self.assertEqual(code, 0, log)
        self.assertEqual(repo.verify(first), [])
        code, second, log = repo.run()
        self.assertEqual(code, 0, log)
        self.assertTrue(any("newer run" in p for p in repo.verify(first)))
        self.assertEqual(repo.verify(second), [])

    def test_9_interrupted_run_and_the_older_success(self):
        # `b` sleeps while a flag outside the repository exists: one
        # commit, a passing run, then a run killed half-way.
        flag = Path(tempfile.mkdtemp()) / "slow"
        slow = f"import os, time\nwhile os.path.exists({str(flag)!r}): time.sleep(0.1)"
        repo = Repo({"a": command(), "b": command(slow, timeout=120)})
        code, good, log = repo.run()
        self.assertEqual(code, 0, log)
        flag.write_text("")
        out = repo.tmp / "killed"
        argv = [sys.executable, str(repo.path / "scripts" / "release-verify.py"), "run", "--ref", "HEAD", "--profile",
                "test", "--out", str(out)]
        p = subprocess.Popen(argv, cwd=repo.path, env=repo.env(), stdout=subprocess.DEVNULL,
                             stderr=subprocess.DEVNULL, start_new_session=True)
        deadline = time.time() + 60
        while time.time() < deadline and not (out / "checks" / "a.json").exists():
            time.sleep(0.05)
        os.killpg(p.pid, signal.SIGKILL)
        p.wait()
        flag.unlink()
        self.assertTrue((out / "checks" / "a.json").exists())
        self.assertFalse((out / "result.json").exists())
        self.assertTrue(any("did not finish" in x for x in repo.verify(out)))
        (out / "RUNNING").unlink()
        self.assertTrue(any("missing: result.json" in x for x in repo.verify(out)))
        # The earlier success is not evidence once a later run of the same
        # commit started and did not pass.
        self.assertTrue(any("newer run" in x for x in repo.verify(good)))
        self.assertEqual(repo.verify(good, allow_superseded=True), [])
        repo.git("worktree", "prune")


    def test_same_second_starts(self):
        repo = Repo({"a": command(), "b": command()})
        code, first, log = repo.run()
        self.assertEqual(code, 0, log)
        res = json.loads((first / "result.json").read_text())
        # A later run of the same commit and profile, logged in the same
        # second (or with the clock set back): the ledger's order decides.
        for when in (res["started_at"], "2000-01-01T00:00:00Z"):
            with open(repo.ledger, "a") as fh:
                fh.write(json.dumps({"run_id": "f" * 32, "commit": res["commit"], "profile": "test",
                                     "started_at": when, "event": "started"}) + "\n")
            self.assertTrue(any("newer run" in p for p in repo.verify(first)), when)

    def test_a_partial_run_does_not_supersede(self):
        repo = Repo({"a": command(), "b": command()})
        code, first, log = repo.run()
        self.assertEqual(code, 0, log)
        code, _, log = repo.run(only=["a"])
        self.assertEqual(repo.verify(first), [])


class Failing(unittest.TestCase):
    def status(self, checks, profile=("a",)):
        repo = Repo(checks, profile)
        code, out, log = repo.run()
        doc = json.loads((out / "checks" / "a.json").read_text())
        return repo, code, out, doc

    def test_10_timeout(self):
        t = time.monotonic()
        repo, code, out, doc = self.status({"a": command("import time; time.sleep(60)", timeout=2)})
        self.assertLess(time.monotonic() - t, 45)
        self.assertEqual((code, doc["status"]), (1, "timeout"))
        self.assertTrue(any("a: timeout" in p for p in repo.verify(out)))

    def test_11_crash(self):
        repo, code, out, doc = self.status({"a": command("import os, signal; os.kill(os.getpid(), signal.SIGSEGV)")})
        self.assertEqual((code, doc["status"], doc["reason"]), (1, "failed", "crashed:command"))
        self.assertTrue(repo.verify(out))

    def test_a_failure_is_not_hidden_by_a_forged_overall_status(self):
        repo, code, out, doc = self.status({"a": command("raise SystemExit(3)"), "b": command()}, ("a", "b"))
        self.assertEqual(doc["status"], "failed")
        edit_result(out, lambda r: r.__setitem__("status", "passed"))
        problems = repo.verify(out)
        self.assertTrue(any("a: failed" in p for p in problems))
        edit_result(out, lambda r: r["checks"]["a"].__setitem__("status", "passed"))
        self.assertTrue(any("status" in p for p in repo.verify(out)))

    def test_12_tool_missing(self):
        repo, code, out, doc = self.status({"a": command(tools=["kasane-no-such-tool"])})
        self.assertEqual((doc["status"], doc["reason"]), ("error", "tool_missing:kasane-no-such-tool"))
        self.assertTrue(repo.verify(out))

    def test_18_worktree_changed(self):
        for code_, why in (("open(r'{src}/README', 'a').write('x')", "worktree:tracked_file_changed"),
                           ("open(r'{src}/new-file', 'w').write('x')", "worktree:untracked_file_added")):
            with self.subTest(why):
                checks = {"a": {"kind": "command", "timeout": 30, "tools": [], "argv": [sys.executable, "-c", code_]}}
                repo, code, out, doc = self.status(checks)
                self.assertEqual((doc["status"], doc["reason"]), ("failed", why))

    def test_18_dirty_checkout_refuses_to_run(self):
        repo = Repo({"a": command()}, ("a",))
        (repo.path / "README").write_text("changed\n")
        code, out, log = repo.run()
        self.assertNotEqual(code, 0)
        self.assertIn("uncommitted changes", log)
        self.assertFalse((out / "result.json").exists())

    def test_partial_run(self):
        repo = Repo({"a": command(), "b": command()})
        code, out, log = repo.run(only=["a"])
        self.assertEqual(code, 0, log)
        problems = repo.verify(out)
        self.assertTrue(any("profile 'partial'" in p for p in problems))
        self.assertTrue(any("b: no result" in p for p in problems))

    def test_a_check_fails_others_still_run(self):
        repo, code, out, doc = self.status({"a": command("raise SystemExit(1)"), "b": command()}, ("a", "b"))
        b = json.loads((out / "checks" / "b.json").read_text())
        self.assertEqual((doc["status"], b["status"]), ("failed", "passed"))

    def test_release_profile_must_hold_every_check(self):
        # A config whose `release` profile has every name but other kinds:
        # its own run passes, the release check does not.
        checks = {n: command() for n in rv.RELEASE_CHECKS}
        repo = Repo(checks, tuple(rv.RELEASE_CHECKS), name="release")
        code, out, log = repo.run(profile="release")
        self.assertEqual(code, 0, log)
        problems = repo.verify(out, profile="release")
        self.assertTrue(any("release profile lacks fuzz (fuzz)" in p for p in problems), problems)

    def test_timeout_kills_descendants(self):
        d = Path(tempfile.mkdtemp())
        pidfile = d / "pid"
        child = d / "child.py"
        child.write_text("import os, signal, sys, time\n"
                         "signal.signal(signal.SIGTERM, signal.SIG_IGN)\n"
                         "open(sys.argv[1], 'w').write(str(os.getpid()))\n"
                         "time.sleep(120)\n")
        code_ = (f"import subprocess, sys, time\n"
                 f"subprocess.Popen([sys.executable, {str(child)!r}, {str(pidfile)!r}])\n"
                 f"time.sleep(120)\n")
        repo, code, out, doc = self.status({"a": command(code_, timeout=3)})
        self.assertEqual(doc["status"], "timeout")
        pid = int(pidfile.read_text())
        time.sleep(0.5)
        with self.assertRaises(ProcessLookupError):
            os.kill(pid, 0)


class Production(unittest.TestCase):
    """The production checks' own judgement, on fake tools."""

    class FakeRunner:
        def __init__(self, text, code=0):
            self.dir = Path(tempfile.mkdtemp())
            self.text, self.code = text, code

        def logfile(self, suffix):
            return self.dir / f"x-{suffix}.log"

        def run(self, argv, cwd=None, env=None, log=None):
            Path(log).write_text(self.text)
            return self.code

    def model(self, text, code=0, seeds=10000):
        try:
            return rv.check_noninterference(self.FakeRunner(text, code), {"seeds": seeds}, None)
        except rv.CheckFailed as e:
            return e.status, e.reason

    def test_model_seed_count(self):
        ok = "model: Tally { ok: 17000, refused: 3000 }\ntest result: ok. 1 passed; 0 failed"
        self.assertEqual(self.model(ok)["ok"], 17000)
        self.assertEqual(self.model(ok.replace("17000", "300").replace("3000", "20")), ("failed", "seeds_mismatch"))
        self.assertEqual(self.model(ok.replace("17000", "0").replace("3000", "20000")), ("failed", "seeds_mismatch"))
        self.assertEqual(self.model("test result: ok. 1 passed; 0 failed"), ("error", "no_summary:redact_model"))
        self.assertEqual(self.model(ok, code=101), ("failed", "exit:redact_model"))
        self.assertEqual(self.model(ok, code=-11), ("failed", "crashed:redact_model"))

    def test_verify_redact_counts_a_crash_as_a_crash(self):
        fake = Path(tempfile.mkdtemp()) / "kasane"
        fake.write_text("#!/usr/bin/env python3\nimport os, signal\nos.kill(os.getpid(), signal.SIGSEGV)\n")
        fake.chmod(0o755)
        r = subprocess.run([sys.executable, str(ROOT / "scripts" / "verify-redact.py"), str(fake)], capture_output=True,
                           text=True)
        if "not found" in r.stdout or "SKIP" in r.stdout:
            self.skipTest("qpdf / poppler not installed")
        self.assertNotEqual(r.returncode, 0)
        self.assertIn("expected refused, got crash", r.stdout)
        self.assertNotRegex(r.stdout, r"\bok +\S+: refused")


class ReleaseCheckLocal(unittest.TestCase):
    """release-check.py's local part: R's and S's evidence, each its own."""

    rc = load("release_check", "scripts/release-check.py")

    def setUp(self):
        self.repo = Repo({"a": command(), "b": command()})
        self.r = self.repo.git("rev-parse", "HEAD")
        code, self.r_out, log = self.repo.run()
        self.assertEqual(code, 0, log)
        (self.repo.path / "tests").mkdir()
        (self.repo.path / "tests" / "advisory.rs").write_text("// new regression\n")
        (self.repo.path / "CHANGELOG.md").write_text("### Security\n")
        self.repo.git("add", "-A")
        self.repo.git("commit", "-qm", "S")
        self.s = self.repo.git("rev-parse", "HEAD")
        code, self.s_out, log = self.repo.run(ref=self.s)
        self.assertEqual(code, 0, log)

    def problems(self, *args, **kw):
        os.environ["KASANE_RELEASE_VERIFY_LEDGER"] = str(self.repo.ledger)
        try:
            return self.rc.local_problems(self.repo.path, *args, profiles=("test", "test", "test"), **kw)
        finally:
            os.environ.pop("KASANE_RELEASE_VERIFY_LEDGER")

    def test_r_and_s(self):
        self.assertEqual(self.problems(self.r, self.r_out), [])
        code, hist, log = self.repo.run(ref=self.s)
        self.assertEqual(code, 0, log)
        # (one profile in this throwaway repository: the newer run of S
        # supersedes the older, so both arguments use it)
        self.assertEqual(self.problems(self.r, self.r_out, self.s, hist, history_results=hist), [])

    def test_security_release_needs_the_history_of_s(self):
        self.assertTrue(any("needs --history-results" in p for p in self.problems(self.r, self.r_out, self.s, self.s_out)))
        self.assertTrue(any("history: results of commit" in p for p in
                            self.problems(self.r, self.r_out, self.s, self.s_out, history_results=self.r_out)))
        self.assertTrue(any("goes with --disclosure" in p for p in self.problems(self.r, self.r_out, history_results=self.s_out)))

    def test_missing_local_results(self):
        self.assertTrue(any("no local verification results" in p for p in self.problems(self.r, None)))

    def test_r_and_s_evidence_swapped(self):
        self.assertTrue(any("local: results of commit" in p for p in self.problems(self.r, self.s_out)))
        self.assertTrue(any("disclosure: results of commit" in p for p in
                            self.problems(self.r, self.r_out, self.s, self.r_out, history_results=self.s_out)))

    def test_s_changing_code(self):
        (self.repo.path / "scripts" / "extra.py").write_text("x = 1\n")
        self.repo.git("add", "-A")
        self.repo.git("commit", "-qm", "S2")
        s2 = self.repo.git("rev-parse", "HEAD")
        code, out, log = self.repo.run(ref=s2)
        self.assertTrue(any("outside tests/" in p for p in self.problems(self.r, self.r_out, s2, out)))

    def test_s_not_after_r(self):
        self.assertTrue(any("does not descend" in p for p in self.problems(self.s, self.s_out, self.r, self.r_out)))
        self.assertTrue(any("go together" in p for p in self.problems(self.r, self.r_out, self.s, None)))

    def test_legacy_commit(self):
        old = self.repo.git("commit-tree", "-m", "old", self.repo.git("hash-object", "-t", "tree", "-w", "/dev/null"))
        self.assertTrue(any("predates" in p for p in self.problems(old, None)))
        self.assertEqual(self.problems(old, None, legacy=True), [])


class Corpus(unittest.TestCase):
    def setUp(self):
        self.src = Path(tempfile.mkdtemp())
        (self.src / "compat").mkdir()
        self.corpus = self.src / "corpus"
        self.corpus.mkdir()
        self.files = {"one": b"%PDF-1.4 one", "two": b"%PDF-1.4 two"}
        lines = []
        for i, data in self.files.items():
            (self.corpus / f"{i}.pdf").write_bytes(data)
            lines += [f"- id: {i}", f"  sha256: {hashlib.sha256(data).hexdigest()}", "  category: c"]
        (self.src / "compat" / "manifest.yaml").write_text("\n".join(lines) + "\n")

    def test_complete(self):
        problem, counts, digest = rv.verify_corpus(self.src, self.corpus)
        self.assertIsNone(problem)
        self.assertEqual(counts, {"files": 2, "missing": 0, "mismatched": 0})
        self.assertTrue(digest)

    def test_14_a_file_missing(self):
        (self.corpus / "two.pdf").unlink()
        problem, counts, _ = rv.verify_corpus(self.src, self.corpus)
        self.assertEqual((problem, counts["missing"]), ("corpus_incomplete", 1))

    def test_14_a_file_changed(self):
        (self.corpus / "two.pdf").write_bytes(b"%PDF-1.4 other")
        problem, counts, _ = rv.verify_corpus(self.src, self.corpus)
        self.assertEqual((problem, counts["mismatched"]), ("corpus_incomplete", 1))

    def test_17_a_corpus_symlink_is_not_read(self):
        outside = self.src / "outside.pdf"
        outside.write_bytes(self.files["two"])
        (self.corpus / "two.pdf").unlink()
        (self.corpus / "two.pdf").symlink_to(outside)
        problem, counts, _ = rv.verify_corpus(self.src, self.corpus)
        self.assertEqual((problem, counts["missing"]), ("corpus_incomplete", 1))


class AdvisoryChecker(unittest.TestCase):
    """check-advisory-regressions.py with a fake CLI: what is not a
    completed, witnessed result is never SAFE."""

    FAKE = """#!/usr/bin/env python3
import os, signal, sys
if len(sys.argv) == 1:
    print("usage: kasane-pdf redact | render | experimental-remove-text")
    sys.exit(2)
out = sys.argv[sys.argv.index("--output") + 1]
if sys.argv[1] == "render":
    os.kill(os.getpid(), signal.SIGKILL)
if sys.argv[1] == "redact":
    open(out, "w").write("not a pdf SECRETX")
    sys.exit(0)
sys.exit(1)
"""

    def test_crash_and_failed_witness(self):
        if shutil.which("qpdf") is None or shutil.which("pdftotext") is None:
            self.skipTest("qpdf / poppler not installed")
        d = Path(tempfile.mkdtemp())
        fake = d / "kasane"
        fake.write_text(self.FAKE)
        fake.chmod(0o755)
        dump = d / "dump"
        dump.mkdir()
        rows = [("a.pdf", "render 1 --dpi 72", "fast"),
                ("b.pdf", "redact --page 1 --rect 1 1 2 2", "safe-bytes:SECRETX"),
                ("c.pdf", "redact --page 1 --rect 1 1 2 2", "safe-text:SECRETX"),
                ("d.pdf", "nosuchcommand 1", "refused")]
        for f, cli, expect in rows:
            (dump / f).write_bytes(b"%PDF-1.4\n")
            (dump / f"manifest-{f[:-4]}.tsv").write_text(f"{f}\tGHSA-test\t{cli}\t{expect}\n")
        r = subprocess.run([sys.executable, str(ROOT / "scripts" / "check-advisory-regressions.py"), str(fake), str(dump),
                            "--expect-safe"], capture_output=True, text=True)
        lines = {l.split()[1]: l for l in r.stdout.splitlines() if l.startswith("GHSA-")}
        self.assertEqual(r.returncode, 1)
        self.assertIn("CRASH", lines["a.pdf"])
        self.assertIn("ERROR (witness)", lines["b.pdf"])
        self.assertIn("ERROR (witness)", lines["c.pdf"])
        self.assertIn("N/A", lines["d.pdf"])
        self.assertNotIn("SAFE", r.stdout)


class EvalWitness(unittest.TestCase):
    def test_unrecognised_mupdf_output_is_not_measured(self):
        ev = load("eval_redact_corpus", "scripts/eval-redact-corpus.py")
        orig = ev.subprocess.run
        ev.subprocess.run = lambda *a, **k: subprocess.CompletedProcess(a, 0, stdout="<garbage/>", stderr="")
        try:
            self.assertIsNone(ev.mupdf_chars("x.pdf", (100, 100)))
        finally:
            ev.subprocess.run = orig


class RedactCorpusCompare(unittest.TestCase):
    """check-redact-corpus.py on projections made by hand."""

    def compare(self, old, new, *flags):
        d = Path(tempfile.mkdtemp())
        (d / "eval-default.json").write_text(json.dumps({"results": old}))
        (d / "new.json").write_text(json.dumps({"results": new}))
        r = subprocess.run([sys.executable, str(ROOT / "scripts" / "check-redact-corpus.py"), "default", str(d / "new.json"),
                            "--baseline", str(d), "--json", str(d / "out.json"), *flags], capture_output=True, text=True)
        return r.returncode, json.loads((d / "out.json").read_text())

    def run_(self, outcome="ok", **m):
        base = {"id": "f", "kind": "word", "category": "c", "outcome": outcome}
        if outcome == "ok":
            base.update({k: 0 for k in ("outside_words", "outside_words_lost", "outside_chars", "outside_chars_lost",
                                        "outside_chars_mupdf", "outside_chars_mupdf_lost", "outside_chars_mupdf_moved",
                                        "unchanged_ink_inside", "outside_pixels_changed")})
        base.update(m)
        return base

    def test_same(self):
        self.assertEqual(self.compare([self.run_()], [self.run_()], "--strict-pixels")[0], 0)

    def test_a_witness_that_did_not_run_is_not_zero(self):
        new = self.run_()
        for k in ("outside_chars_mupdf", "outside_chars_mupdf_lost", "outside_pixels_changed"):
            new.pop(k)
        code, doc = self.compare([self.run_()], [new], "--strict-pixels")
        self.assertEqual((code, doc["regressions"][0]["what"]), (1, "measurement_missing"))

    def test_improvements_do_not_offset_a_regression(self):
        old = [self.run_(), dict(self.run_("refused", reason="tiling pattern"), id="g")]
        new = [self.run_(outside_words_lost=1), dict(self.run_(), id="g")]
        code, doc = self.compare(old, new)
        self.assertEqual(code, 1)
        self.assertEqual((len(doc["regressions"]), len(doc["improvements"])), (1, 1))
        self.assertEqual(doc["counts"]["run"]["ok"], 2)

    def test_the_accepted_image_signal_keeps_its_measurements(self):
        sig = dict(check_by_hand=True, unchanged_ink_not_from_images=0, outside_words=3, outside_words_lost=0,
                   outside_chars_mupdf=5, outside_chars_mupdf_lost=0)
        old = [self.run_("unexpected", **sig)]
        self.assertEqual(self.compare(old, [self.run_("unexpected", **sig)])[0], 0)
        gone = dict(sig)
        gone.pop("outside_chars_mupdf")
        gone.pop("outside_chars_mupdf_lost")
        code, doc = self.compare(old, [self.run_("unexpected", **gone)])
        self.assertEqual((code, doc["regressions"][0]["what"]), (1, "measurement_missing"))

    def test_more_pixels_changed_only_when_strict(self):
        old, new = [self.run_()], [self.run_(outside_pixels_changed=4)]
        self.assertEqual(self.compare(old, new)[0], 0)
        self.assertEqual(self.compare(old, new, "--strict-pixels")[0], 1)


class Renderers(unittest.TestCase):
    """verify-renderers.py's judgement, on results made by hand."""

    VERSIONS = {"poppler": "26.10.0", "mupdf": "1.28.6", "pdfjs": "6.5.438"}

    def results(self, **renderer):
        got = {"ink_left": 0, "changed": 2, "antialias": 10, "secret_extracted": False, "public_lost": False}
        got.update(renderer)
        return {"vr:text": {"outcome": "ok", "expected": "ok", "renderers": {r: dict(got) for r in vrend.RENDERERS}}}

    def baseline(self, versions=None):
        return {"tools": versions or dict(self.VERSIONS),
                "cases": {"vr:text": {"outcome": "ok", "renderers": {r: {"changed": 2} for r in vrend.RENDERERS}}}}

    def test_clean(self):
        self.assertEqual(vrend.judge(self.results(), self.VERSIONS, self.baseline()), ([], []))

    def test_13_another_renderer_version_is_incomparable(self):
        versions = dict(self.VERSIONS, mupdf="1.29.0")
        problems, incomparable = vrend.judge(self.results(), versions, self.baseline())
        self.assertEqual((problems, incomparable), ([], ["mupdf"]))

    def test_a_leak_fails_even_when_incomparable(self):
        versions = dict(self.VERSIONS, mupdf="1.29.0")
        for kw, kind in (({"ink_left": 3}, "leak_ink"), ({"secret_extracted": True}, "leak_text"),
                         ({"public_lost": True}, "loss_text")):
            problems, _ = vrend.judge(self.results(**kw), versions, self.baseline())
            self.assertTrue(any(p.startswith(kind) for p in problems), kw)

    def test_more_drawing_lost_than_the_baseline(self):
        problems, _ = vrend.judge(self.results(changed=3), self.VERSIONS, self.baseline())
        self.assertTrue(any(p.startswith("loss_drawing") for p in problems))
        problems, _ = vrend.judge(self.results(changed=1, antialias=500), self.VERSIONS, self.baseline())
        self.assertEqual(problems, [])

    def test_outcomes(self):
        r = self.results()
        r["vr:text"]["outcome"] = "crash"
        self.assertTrue(any(p.startswith("crash") for p in vrend.judge(r, self.VERSIONS, self.baseline())[0]))
        r = self.results()
        r["vr:text"]["outcome"] = "refused"
        problems = vrend.judge(r, self.VERSIONS, self.baseline())[0]
        self.assertTrue(any(p.startswith("outcome_not_expected") for p in problems))
        self.assertTrue(any(p.startswith("outcome_changed") for p in problems))
        b = self.baseline()
        b["cases"]["vr:gone"] = b["cases"]["vr:text"]
        self.assertTrue(any(p.startswith("case_missing") for p in vrend.judge(self.results(), self.VERSIONS, b)[0]))

    def test_a_renderer_missing_or_unread(self):
        r = self.results()
        r["vr:text"]["renderers"].pop("pdfjs")
        self.assertIn("renderer_missing:vr:text:pdfjs", vrend.judge(r, self.VERSIONS, self.baseline())[0])
        r = self.results()
        r["vr:text"]["renderers"]["mupdf"] = {"error": "text_failed"}
        self.assertIn("text_failed:vr:text:mupdf", vrend.judge(r, self.VERSIONS, self.baseline())[0])

    def test_input_text_unreadable_is_an_error(self):
        orig_render, orig_text = vrend.render, vrend.text
        white = (10, 10, bytes([255] * 100))
        vrend.render = lambda rd, pdf, base: white
        vrend.text = lambda rd, pdf: None if pdf.endswith("in.pdf") else ""
        try:
            case = ("vr:t", b"%PDF", ["--page", "1"], [1, 1, 5, 5], "SECRETTEXT", "PUBLICWORD", False, "ok")
            fake = Path(tempfile.mkdtemp()) / "kasane"
            fake.write_text("#!/bin/sh\nfor a; do o=$a; done\ncp \"$2\" \"$o\"\n")
            fake.chmod(0o755)
            _, res = vrend.run_case(str(fake), case, vrend.RENDERERS)
        finally:
            vrend.render, vrend.text = orig_render, orig_text
        self.assertEqual(res["outcome"], "ok")
        self.assertTrue(all(g.get("error") == "text_failed" for g in res["renderers"].values()), res)

    def test_pixel_classes(self):
        w = h = 20
        a = (20, 20, bytes([255] * (w * h)))
        b = bytearray(a[2])
        b[0] = 240          # outside, small change: antialias
        b[1] = 0            # outside, large change: changed
        b[10 * w + 10] = 0  # inside, newly drawn: not ink left
        rect = [5, 5, 15, 15]
        c = vrend.compare(a, (w, h, bytes(b)), rect, False)
        self.assertEqual((c["antialias"], c["changed"], c["ink_left"]), (1, 1, 0))
        a2 = bytearray(a[2])
        a2[10 * w + 10] = 0
        c = vrend.compare((w, h, bytes(a2)), (w, h, bytes(a2)), rect, False)
        self.assertEqual(c["ink_left"], 1)
        self.assertIsNone(vrend.compare(a, (w + 1, h, bytes([255] * ((w + 1) * h))), rect, False))


if __name__ == "__main__":
    unittest.main(verbosity=1)
