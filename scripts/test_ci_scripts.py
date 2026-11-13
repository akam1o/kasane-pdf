#!/usr/bin/env python3
"""Tests of the CI's own decisions (pr.yml compat-smoke):

  * scripts/ci-gate.py — the gate passes only when every job succeeded,
    a full-gate job skipped on a pull request aside;
  * scripts/check-workflows.py — each weakening of the gate it is meant
    to catch is caught, and the committed workflows pass;
  * scripts/release-check.py — only a successful full gate on the
    exact commit (and, when required, its nightly) lets it be released;
  * fuzz/ci-run-sharded.sh — a failing, killed or missing shard fails
    the run, the seeds are all dealt (file names with spaces included),
    and bad arguments, no seeds or an empty shard are refused.

    python3 scripts/test_ci_scripts.py
"""
import contextlib
import importlib.util
import io
import json
import sys
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

import yaml

ROOT = Path(__file__).resolve().parent.parent


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, ROOT / file)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


gate = load("ci_gate", "scripts/ci-gate.py")
cw = load("check_workflows", "scripts/check-workflows.py")
PR = ROOT / ".github/workflows/pr.yml"
GUARD = ROOT / ".github/workflows/pr-guard.yml"
OTHERS = [p for p in sorted((ROOT / ".github/workflows").glob("*.yml")) if p not in (PR, GUARD)]


class Gate(unittest.TestCase):
    def jobs(self):
        return sorted(yaml.safe_load(PR.read_text())["jobs"]["gate"]["needs"])

    def test_every_combination(self):
        for event in ("pull_request", "push", "workflow_dispatch"):
            for job in self.jobs():
                for result in ("success", "failure", "cancelled", "skipped"):
                    needs = {j: {"result": "success"} for j in self.jobs()}
                    needs[job] = {"result": result}
                    passes = not gate.failures(needs, event)
                    expect = result == "success" or (result == "skipped" and event == "pull_request" and job in gate.FULL)
                    self.assertEqual(passes, expect, (event, job, result))

    def test_no_jobs_is_not_a_pass(self):
        self.assertTrue(gate.failures({}, "push"))

    def test_missing_unknown_and_unexpected_results(self):
        ok = {j: {"result": "success"} for j in self.jobs()}
        for event in ("pull_request", "push"):
            missing = dict(ok)
            missing.pop("compat-smoke")
            self.assertIn("compat-smoke", gate.failures(missing, event))
            unknown = dict(ok, engine={})
            self.assertIn("engine", gate.failures(unknown, event))
            null = dict(ok, engine={"result": None})
            self.assertIn("engine", gate.failures(null, event))
            extra = dict(ok, renamed={"result": "success"})
            self.assertIn("renamed", gate.failures(extra, event))

    def test_expected_is_the_workflow(self):
        self.assertEqual(set(self.jobs()), set(gate.EXPECTED))


class Workflows(unittest.TestCase):
    def problems(self, mutate, mutate_guard=None, extra=None, drop_pr=False):
        wf = yaml.safe_load(PR.read_text())
        guard = yaml.safe_load(GUARD.read_text())
        mutate(wf)
        if mutate_guard:
            mutate_guard(guard)
        cw.problems.clear()
        with contextlib.redirect_stdout(io.StringIO()):
            cw.check_gate(PR, wf)
            cw.check_caches(PR, wf)
            cw.check_guard(GUARD, guard)
            wfs = ([] if drop_pr else [(PR, wf)]) + [(GUARD, guard)] + [(p, yaml.safe_load(p.read_text())) for p in OTHERS]
            if extra:
                wfs.append(extra)
            cw.check_names(wfs)
        out = list(cw.problems)
        cw.problems.clear()
        return out

    def step(self, wf, job, text):
        return next(st for st in wf["jobs"][job]["steps"] if text in (st.get("run") or ""))

    def test_committed_workflows_pass(self):
        self.assertEqual(self.problems(lambda wf: None), [])

    def test_weakenings_are_caught(self):
        on = lambda wf: wf.get("on") if "on" in wf else wf[True]
        cases = {
            "a security step switched off": lambda wf: self.step(wf, "compat-smoke", "verify-redact.py").__setitem__("if", "false"),
            "the advisory check switched off": lambda wf: self.step(wf, "compat-smoke", "--expect-safe").__setitem__("if", "false"),
            "--expect-safe dropped": lambda wf: self.step(wf, "compat-smoke", "--expect-safe").__setitem__(
                "run", self.step(wf, "compat-smoke", "--expect-safe")["run"].replace(" --expect-safe", "")),
            "the engine tests switched off": lambda wf: self.step(wf, "engine", "cargo nextest run").__setitem__("if", "false"),
            "doctests dropped": lambda wf: wf["jobs"]["engine"]["steps"].remove(self.step(wf, "engine", "cargo test --doc")),
            "a fuzz step for a target the matrix lacks": lambda wf: self.step(wf, "render-fuzz-smoke", "ci-run-sharded.sh").__setitem__(
                "if", "matrix.target == 'nothing'"),
            "the security job made full-gate only": lambda wf: wf["jobs"]["compat-smoke"].__setitem__("if", cw.FULL_ONLY_IF),
            "a job outside the gate": lambda wf: wf["jobs"]["gate"]["needs"].remove("wasm"),
            "the gate not always()": lambda wf: wf["jobs"]["gate"].pop("if"),
            "the gate not deciding": lambda wf: self.step(wf, "gate", "ci-gate.py").__setitem__("run", "true"),
            "a full-gate job ci-gate.py does not know": lambda wf: wf["jobs"]["wasm"].__setitem__("if", cw.FULL_ONLY_IF),
            "a timeout removed": lambda wf: wf["jobs"]["engine"].pop("timeout-minutes"),
            "continue-on-error": lambda wf: wf["jobs"]["engine"].__setitem__("continue-on-error", True),
            "a cache written from pull requests": lambda wf: next(
                st for st in wf["jobs"]["engine"]["steps"] if "rust-cache" in (st.get("uses") or ""))["with"].__setitem__(
                "save-if", "${{ github.ref != 'refs/heads/main' }}"),
            "dispatch removed": lambda wf: on(wf).pop("workflow_dispatch"),
            "pushes to main not run": lambda wf: on(wf)["push"].__setitem__("branches", ["other"]),
            "a required check renamed": lambda wf: wf["jobs"]["engine"].__setitem__("name", "tests"),
            "the gate's decision switched off by `if: false`": lambda wf: self.step(wf, "gate", "ci-gate.py").__setitem__("if", False),
            "a fuzz target dropped from the matrix": lambda wf: wf["jobs"]["render-fuzz-smoke"]["strategy"]["matrix"]["include"].pop(1),
            "redact_model shards left uncovered": lambda wf: wf["jobs"]["render-fuzz-smoke"]["strategy"]["matrix"]["include"].pop(3),
            "a cache saved from any ref (`|| true`)": lambda wf: next(
                st for st in wf["jobs"]["engine"]["steps"] if "rust-cache" in (st.get("uses") or ""))["with"].__setitem__(
                "save-if", "${{ github.ref == 'refs/heads/main' || true }}"),
        }
        new_job = {"name": "new check", "runs-on": "ubuntu-latest", "timeout-minutes": 5, "steps": [{"run": "true"}]}

        def add_job(wf, in_gate):
            wf["jobs"]["new-check"] = dict(new_job)
            if in_gate:
                wf["jobs"]["gate"]["needs"].append("new-check")

        cases.update({
            "pull requests filtered by path": lambda wf: on(wf).__setitem__("pull_request", {"paths": ["src/**"]}),
            "the gate renamed": lambda wf: wf["jobs"]["gate"].__setitem__("name", "Gate"),
            "a new job outside the gate": lambda wf: add_job(wf, False),
            "a new job the gate script does not expect": lambda wf: add_job(wf, True),
        })
        for name, mutate in cases.items():
            with self.subTest(name):
                self.assertTrue(self.problems(mutate), f"not caught: {name}")
        nothing = lambda wf: None
        guard_cases = {
            "the guard may write": lambda g: g.__setitem__("permissions", {"contents": "write"}),
            "the guard checks out the pull request": lambda g: g["jobs"]["guard"]["steps"][0]["with"].__setitem__(
                "ref", "${{ github.event.pull_request.head.sha }}"),
            "the guard runs another action": lambda g: g["jobs"]["guard"]["steps"].append({"uses": "x/y@" + "0" * 40}),
            "the guard on pull_request": lambda g: g.__setitem__("on", {"pull_request": None}),
            "the guard not checking": lambda g: g["jobs"]["guard"]["steps"].pop(2),
        }
        for name, mutate in guard_cases.items():
            with self.subTest(name):
                self.assertTrue(self.problems(nothing, mutate_guard=mutate), f"not caught: {name}")
        def run_of(wf, job, text):
            return self.step(wf, job, text)

        def guard_step(g, text):
            return next(st for st in g["jobs"]["guard"]["steps"] if text in (st.get("run") or ""))

        later = {
            "the gate's command printed, not run": lambda wf: run_of(wf, "gate", "ci-gate.py").__setitem__(
                "run", "echo scripts/ci-gate.py"),
            "the gate's command made to pass": lambda wf: run_of(wf, "gate", "ci-gate.py").__setitem__(
                "run", "python3 scripts/ci-gate.py || true"),
            "the gate's results not passed in": lambda wf: run_of(wf, "gate", "ci-gate.py").__setitem__(
                "env", {"NEEDS": "{}", "EVENT": "pull_request"}),
            "clippy printed, not run": lambda wf: run_of(wf, "engine", "cargo clippy --all-targets").__setitem__(
                "run", "echo cargo clippy --all-targets -- -D warnings"),
            "the advisory check made to pass": lambda wf: run_of(wf, "compat-smoke", "--expect-safe").__setitem__(
                "run", run_of(wf, "compat-smoke", "--expect-safe")["run"].replace("--expect-safe", "--expect-safe || true")),
            "a job named by an expression that can be `gate`": lambda wf: wf["jobs"]["wasm"].__setitem__(
                "name", "${{ 'gate' }}"),
        }
        for name, mutate in later.items():
            with self.subTest(name):
                self.assertTrue(self.problems(mutate), f"not caught: {name}")
        later_guard = {
            "the guard job switched off": lambda g: g["jobs"]["guard"].__setitem__("if", "false"),
            "the guard's data check switched off": lambda g: guard_step(g, "--data-only data").__setitem__("if", "false"),
            "the fork check removed": lambda g: g["jobs"]["guard"]["steps"].remove(guard_step(g, "--fork-changes")),
            "the fork check made to pass": lambda g: guard_step(g, "--fork-changes").__setitem__(
                "run", guard_step(g, "--fork-changes")["run"] + " || true"),
            "the guard's checkout shallow": lambda g: g["jobs"]["guard"]["steps"][0]["with"].pop("fetch-depth"),
            "the guard's checkout elsewhere": lambda g: g["jobs"]["guard"]["steps"][0]["with"].__setitem__("path", "."),
            "the guard checks out by a lookalike action": lambda g: g["jobs"]["guard"]["steps"][0].__setitem__(
                "uses", "evil/actions/checkout@" + "0" * 40),
        }
        for name, mutate in later_guard.items():
            with self.subTest(name):
                self.assertTrue(self.problems(nothing, mutate_guard=mutate), f"not caught: {name}")
        with self.subTest("pr.yml replaced by another workflow with the required names"):
            fake = {"on": {"pull_request": None}, "jobs": {
                "a": {"name": "test / fmt / clippy", "steps": [{"run": "true"}]},
                "b": {"name": "compat + round-trip smoke (fixtures)", "steps": [{"run": "true"}]},
                "c": {"name": "gate", "steps": [{"run": "true"}]}}}
            self.assertTrue(self.problems(nothing, extra=(Path("replacement.yml"), fake), drop_pr=True))
        with self.subTest("a job named `gate` in another workflow"):
            other = {"on": {"pull_request": None}, "jobs": {"x": {"name": "gate", "if": "false", "steps": [{"run": "true"}]}}}
            self.assertTrue(self.problems(nothing, extra=(Path("other.yml"), other)))


class DataOnly(unittest.TestCase):
    """check-workflows.py --data-only on a copy of the tree, and the guard's
    fork decision on the paths git really reports."""

    def tree(self):
        d = Path(tempfile.mkdtemp())
        for rel in [".github/workflows/" + p.name for p in (ROOT / ".github/workflows").iterdir()] + [
                "scripts/ci-gate.py", "compat/fetch-corpus.py", "scripts/cjk-assets.sha256", "scripts/fetch-cjk-assets.sh"]:
            (d / rel).parent.mkdir(parents=True, exist_ok=True)
            (d / rel).write_bytes((ROOT / rel).read_bytes())
        return d

    def check(self, d):
        return subprocess.run([sys.executable, str(ROOT / "scripts/check-workflows.py"), "--data-only", str(d)],
                              capture_output=True, text=True)

    def test_the_tree_passes(self):
        r = self.check(self.tree())
        self.assertEqual(r.returncode, 0, r.stdout)

    def test_a_yaml_workflow_with_a_gate(self):
        d = self.tree()
        (d / ".github/workflows/duplicate.yaml").write_text(
            "on: pull_request\njobs:\n  x:\n    name: gate\n    runs-on: ubuntu-latest\n    steps:\n      - run: 'true'\n")
        self.assertNotEqual(self.check(d).returncode, 0)

    def test_pr_yml_removed(self):
        d = self.tree()
        (d / ".github/workflows/pr.yml").unlink()
        self.assertNotEqual(self.check(d).returncode, 0)

    def git(self, d, *args):
        subprocess.run(["git", "-C", str(d), *args], check=True, capture_output=True)

    def changed(self, setup):
        d = Path(tempfile.mkdtemp())
        self.git(d, "init", "-q", "-b", "main")
        self.git(d, "config", "user.email", "t@example.invalid")
        self.git(d, "config", "user.name", "t")
        (d / "scripts").mkdir()
        (d / "scripts/check.py").write_text("x\n")
        (d / "README").write_text("r\n")
        self.git(d, "add", "-A")
        self.git(d, "commit", "-qm", "base")
        self.git(d, "checkout", "-qb", "pr")
        setup(d)
        self.git(d, "add", "-A")
        self.git(d, "commit", "-qm", "pr")
        out = subprocess.run(["git", "-C", str(d), "diff", "--no-renames", "--name-only", "-z", "main...pr"],
                             capture_output=True, check=True).stdout
        return cw.ci_changes(out)

    def test_a_rename_out_of_a_ci_path(self):
        def rename(d):
            (d / "compat").mkdir()
            (d / "scripts/check.py").rename(d / "compat/check.py")
        self.assertEqual(self.changed(rename), [b"scripts/check.py"])

    def test_unusual_names(self):
        for name in ["scripts/check-\u00e9.py", "scripts/a\tb.py", "scripts/a\nb.py", ".github/workflows/x y.yml"]:
            with self.subTest(name=name):
                def add(d, name=name):
                    (d / name).parent.mkdir(parents=True, exist_ok=True)
                    (d / name).write_text("x\n")
                self.assertEqual(self.changed(add), [name.encode()])

    def test_other_paths_are_not_ci(self):
        self.assertEqual(self.changed(lambda d: (d / "README").write_text("changed\n")), [])

    def test_fork_decision(self):
        f = Path(tempfile.mkdtemp()) / "changed.z"
        cli = lambda fork: subprocess.run([sys.executable, str(ROOT / "scripts/check-workflows.py"), "--fork-changes",
                                           str(f), fork], capture_output=True, text=True).returncode
        f.write_bytes(b"src/lib.rs\0scripts/verify-redact.py\0")
        self.assertNotEqual(cli("true"), 0)
        self.assertEqual(cli("false"), 0)
        f.write_bytes(b"src/lib.rs\0docs/x.md\0")
        self.assertEqual(cli("true"), 0)


class ReleaseCheck(unittest.TestCase):
    """scripts/release-check.py: only a full gate, CodeQL and (when
    required) a nightly whose corpus job really ran, on the exact commit."""

    rc = load("release_check", "scripts/release-check.py")
    SHA = "a" * 40
    CORPUS_OK = {"name": "full compatibility + round-trip (511)", "conclusion": "success", "steps": [
        {"name": "redaction corpus — four option sets", "conclusion": "success"},
        {"name": "full run (all pages)", "conclusion": "success"},
        {"name": "regression check and quality gate", "conclusion": "success"}]}

    def run_(self, rid, path="pr.yml", event="push", conclusion="success", sha=SHA):
        if "/" not in path:
            path = f".github/workflows/{path}"
        return {"id": rid, "path": path, "event": event, "conclusion": conclusion, "head_sha": sha}

    def gate(self, conclusion="success"):
        return [{"name": "gate", "conclusion": conclusion}]

    def base(self):
        return [self.run_(1), self.run_(9, path="dynamic/github-code-scanning/codeql", event="dynamic")], {1: self.gate()}

    def problems(self, runs, jobs, ancestor=True, nightly=False):
        return self.rc.verdict(self.SHA, runs, jobs, ancestor, nightly)

    def test_full_gate_and_codeql_on_the_commit_pass(self):
        runs, jobs = self.base()
        self.assertEqual(self.problems(runs, jobs), [])
        dispatch = [self.run_(1, event="workflow_dispatch"), self.run_(9, path="codeql-release.yml")]
        self.assertEqual(self.problems(dispatch, jobs), [])

    def test_what_does_not_count(self):
        codeql = self.run_(9, path="dynamic/github-code-scanning/codeql", event="dynamic")
        cases = {
            "a pull request's run (fast gate)": ([self.run_(1, event="pull_request"), codeql], {1: self.gate()}),
            "a failed run": ([self.run_(1, conclusion="failure"), codeql], {1: self.gate("failure")}),
            "a cancelled run": ([self.run_(1, conclusion="cancelled"), codeql], {1: self.gate("cancelled")}),
            "a run still going": ([self.run_(1, conclusion=None), codeql], {1: self.gate(None)}),
            "gate skipped": ([self.run_(1), codeql], {1: self.gate("skipped")}),
            "no gate job": ([self.run_(1), codeql], {1: []}),
            "another commit's run": ([self.run_(1, sha="b" * 40), codeql], {1: self.gate()}),
            "another workflow": ([self.run_(1, path="nightly.yml"), codeql], {1: self.gate()}),
            "no CodeQL": ([self.run_(1)], {1: self.gate()}),
            "CodeQL failed": ([self.run_(1), self.run_(9, path="codeql-release.yml", conclusion="failure")], {1: self.gate()}),
            "CodeQL of another commit": ([self.run_(1), self.run_(9, path="codeql-release.yml", sha="b" * 40)], {1: self.gate()}),
            "no run at all": ([], {}),
        }
        for name, (runs, jobs) in cases.items():
            with self.subTest(name):
                self.assertTrue(self.problems(runs, jobs), name)

    def test_main_must_fast_forward(self):
        runs, jobs = self.base()
        self.assertTrue(self.problems(runs, jobs, ancestor=False))

    def test_nightly_corpus_on_the_commit_when_required(self):
        runs, jobs = self.base()
        self.assertTrue(self.problems(runs, jobs, nightly=True))
        skipped = dict(self.CORPUS_OK, conclusion="skipped", steps=[])
        partial = dict(self.CORPUS_OK, steps=self.CORPUS_OK["steps"][1:])
        cases = {
            "another commit's nightly": (self.run_(2, path="nightly.yml", sha="b" * 40), [self.CORPUS_OK]),
            "a failed nightly": (self.run_(2, path="nightly.yml", conclusion="failure"), [self.CORPUS_OK]),
            "a green nightly whose corpus job was skipped": (self.run_(2, path="nightly.yml"), [skipped]),
            "a green nightly without the corpus job": (self.run_(2, path="nightly.yml"), []),
            "the redaction corpus step missing": (self.run_(2, path="nightly.yml"), [partial]),
        }
        for name, (run, njobs) in cases.items():
            with self.subTest(name):
                self.assertTrue(self.problems(runs + [run], {**jobs, 2: njobs}, nightly=True), name)
        good = runs + [self.run_(2, path="nightly.yml", event="workflow_dispatch")]
        self.assertEqual(self.problems(good, {**jobs, 2: [self.CORPUS_OK]}, nightly=True), [])


class Projection(unittest.TestCase):
    """scripts/redact-results-projection.py keeps no free text."""

    FREE_TEXT = "Quarterly figures for ACME"

    def project(self, data):
        d = Path(tempfile.mkdtemp())
        (d / "in.json").write_text(json.dumps(data))
        r = subprocess.run([sys.executable, str(ROOT / "scripts/redact-results-projection.py"), str(d / "in.json"), str(d / "out.json")],
                           capture_output=True, text=True)
        self.assertEqual(r.returncode, 0, r.stderr)
        return (d / "out.json").read_text()

    def test_eval_results(self):
        out = self.project({"results": [
            {"id": "f1", "kind": "word", "category": "c", "outcome": "unexpected", "reason": self.FREE_TEXT,
             "detail": self.FREE_TEXT, "producer": self.FREE_TEXT, "rect": [1, 2, 3, 4], "unknown_count": 7,
             "check_by_hand": self.FREE_TEXT, "outside_words_lost": 2},
            {"id": "f2", "kind": "line", "category": "c", "outcome": self.FREE_TEXT}],
            "skipped": [["f3", self.FREE_TEXT]]})
        self.assertNotIn(self.FREE_TEXT, out)
        self.assertNotIn("unknown_count", out)
        self.assertNotIn("rect", out)
        p = json.loads(out)["results"]
        self.assertEqual((p[0]["outcome"], p[0]["check_by_hand"], p[0]["outside_words_lost"]), ("unexpected", True, 2))
        self.assertEqual(p[1]["outcome"], "unexpected")

    def test_refusal_results(self):
        out = self.project({"summary": {"refused_runs": 2, "first_reasons": {self.FREE_TEXT: 1, "JavaScript name tree": 1}},
                            "runs": [{"id": "f1", "kind": "word", "first_reason": self.FREE_TEXT, "first_detail": self.FREE_TEXT,
                                      "walk_error": self.FREE_TEXT, "walk_categories": [self.FREE_TEXT, "tiling pattern"],
                                      "examples": {"x": self.FREE_TEXT}, "rect": [1, 2, 3, 4]}]})
        self.assertNotIn(self.FREE_TEXT, out)
        p = json.loads(out)
        self.assertEqual(p["runs"][0]["first_reason"], "other")
        self.assertEqual(p["runs"][0]["walk_categories"], ["other", "tiling pattern"])
        self.assertEqual(p["summary"]["first_reasons"], {"other": 1, "JavaScript name tree": 1})


class Shards(unittest.TestCase):
    """fuzz/ci-run-sharded.sh with a stub in place of fuzz/ci-run.sh."""

    STUB = """#!/usr/bin/env bash
dir=$2
k=$(basename "$dir")
ls "$dir" >> "$STUB_LOG"
case "${STUB_MODE:-ok}:$k" in
  fail:shard-2) echo "ci-run: x status=1"; exit 1 ;;
  kill:shard-1) kill -9 $$ ;;
esac
echo "ci-run: x status=0 finished=1"
echo "Done 1 runs in 0 second(s)"
"""

    def run_sharded(self, seeds, args, mode="ok"):
        d = Path(tempfile.mkdtemp())
        sd = d / "seeds"
        sd.mkdir()
        for name in seeds:
            (sd / name).write_text(name)
        stub = d / "stub.sh"
        stub.write_text(self.STUB)
        stub.chmod(0o755)
        log = d / "log"
        log.write_text("")
        env = dict(os.environ, FUZZ_CI_RUN=str(stub), STUB_LOG=str(log), STUB_MODE=mode)
        r = subprocess.run(["bash", str(ROOT / "fuzz/ci-run-sharded.sh"), "t", str(sd), *map(str, args)],
                           capture_output=True, text=True, env=env)
        return r.returncode, r.stdout + r.stderr, [ln for ln in log.read_text().splitlines() if ln]

    def test_all_seeds_dealt_names_with_spaces(self):
        seeds = ["a b", "c", "d e f", "g", "h"]
        code, out, dealt = self.run_sharded(seeds, [2, 5, 10])
        self.assertEqual(code, 0, out)
        self.assertEqual(sorted(dealt), sorted(seeds))

    def test_a_range_of_shards(self):
        code, out, dealt = self.run_sharded(["1", "2", "3", "4"], [4, 5, 10, 3, 4])
        self.assertEqual(code, 0, out)
        self.assertEqual(sorted(dealt), ["3", "4"])

    def test_a_failing_shard_fails(self):
        code, out, _ = self.run_sharded(["1", "2", "3"], [3, 5, 10], mode="fail")
        self.assertNotEqual(code, 0, out)

    def test_a_killed_shard_fails(self):
        code, out, _ = self.run_sharded(["1", "2", "3"], [3, 5, 10], mode="kill")
        self.assertNotEqual(code, 0, out)

    def test_refused_setups(self):
        for seeds, args in [
            ([], [1, 5, 10]),                       # no seeds
            (["1", "2"], [3, 5, 10]),               # a shard would be empty
            (["1", "2", "3"], [3, 5, 10, 3, 2]),    # first > last
            (["1", "2", "3"], [3, 5, 10, 0, 2]),    # first < 1
            (["1", "2", "3"], [3, 5, 10, 2, 4]),    # last > shards
            (["1", "2", "3"], [0, 5, 10]),          # no shards
            (["1", "2", "3"], [3, "x", 10]),        # not a number
            (["1", "2", "3"], [3, 5]),              # missing argument
        ]:
            with self.subTest(seeds=seeds, args=args):
                code, out, _ = self.run_sharded(seeds, args)
                self.assertNotEqual(code, 0, out)


if __name__ == "__main__":
    unittest.main(verbosity=1)
