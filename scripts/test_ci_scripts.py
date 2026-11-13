#!/usr/bin/env python3
"""Tests of the CI's own decisions (pr.yml compat-smoke):

  * scripts/ci-gate.py — the gate passes only when every job succeeded,
    a full-gate job skipped on a pull request aside;
  * scripts/check-workflows.py — each weakening of the gate it is meant
    to catch is caught, and the committed workflows pass;
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


class Workflows(unittest.TestCase):
    def problems(self, mutate):
        wf = yaml.safe_load(PR.read_text())
        mutate(wf)
        cw.problems.clear()
        with contextlib.redirect_stdout(io.StringIO()):
            cw.check_gate(PR, wf)
            cw.check_caches(PR, wf)
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
        for name, mutate in cases.items():
            with self.subTest(name):
                self.assertTrue(self.problems(mutate), f"not caught: {name}")


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
