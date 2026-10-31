#!/usr/bin/env python3
"""Tests for the exact-render quality gate in ci-regression.py.

    python3 scripts/test_ci_regression.py

Synthetic report records only (no corpus): each case is a measurement
the gate saw on the real corpus, reduced to the fields it reads —
the garbled FontMatrix page before and after the fix, the known
hairline grid, a `/UserUnit` page Poppler renders at the wrong size,
a JBIG2 page Poppler leaves blank, a near-empty page, and a new
misrender.
"""
import contextlib
import importlib.util
import io
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("ci_regression", os.path.join(HERE, "ci-regression.py"))
cr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cr)


def page(ours, ref, cssim=0.8, diff=0.01, sizes=(612, 792, 612, 792)):
    return {
        "quality": {
            "ink_ours": ours,
            "ink_ref": ref,
            "content_ssim": cssim,
            "diff_px_at_best_shift": diff,
            "sizes": list(sizes),
        }
    }


def file(fid, pages, approximate=False, verdict="pass"):
    return {
        "id": fid,
        "expect": "pass",
        "verdict": verdict,
        "parse": {"class": "pass"},
        "render": {"class": "pass"},
        "text": {"class": "pass"},
        "rewrite": {"class": "pass"},
        "recovered": False,
        "render_approximate": approximate,
        "metrics": {"render_pages": pages},
    }


GARBLED = page(0.038, 0.012, cssim=0.07, diff=0.04)  # ratio 3.18
FIXED = page(0.0122, 0.012, cssim=0.88)  # ratio 1.02
GRID = page(0.028, 0.112, cssim=0.5)  # ratio 0.25, Poppler darker
USERUNIT = page(0.02, 0.08, sizes=(1588, 2246, 794, 1123))  # Poppler ignores /UserUnit
JBIG2_BLANK = page(0.05, 0.0001)  # Poppler drew nothing
NEAR_EMPTY = page(0.0009, 0.0002)  # below min_ink on both sides
NORMAL = page(0.10, 0.098)


def run(*files):
    return {"files": list(files)}


def check(r, base, summary=None):
    out = io.StringIO()
    with contextlib.redirect_stdout(out):
        rc = cr.check(r, base, summary)
    return rc, out.getvalue()


class Gate(unittest.TestCase):
    def setUp(self):
        self.gate = cr.load_gate()

    def q(self, *pages, **kw):
        return cr.quality(file("x", list(pages), **kw), self.gate)

    def test_1_fontmatrix_misrender_is_flagged(self):
        self.assertEqual(self.q(NORMAL, GARBLED), "flag:ink_ratio")

    def test_2_fixed_page_passes(self):
        self.assertEqual(self.q(NORMAL, FIXED), "ok")

    def test_3_known_grid_flag_is_listed_not_failed(self):
        base = cr.summarize(run(file("grid", [GRID]), file("a", [NORMAL])))
        rc, out = check(run(file("grid", [GRID]), file("a", [NORMAL])), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("known quality flag", out)
        # The same file with a second flagged page is a new problem.
        rc, out = check(run(file("grid", [GRID, GARBLED]), file("a", [NORMAL])), base)
        self.assertEqual(rc, 1, out)
        self.assertIn("2 flagged pages, 1 in the baseline", out)

    def test_3b_baseline_cannot_absorb_unreviewed_flags(self):
        reviewed = self.gate["reviewed_known_flags"]
        flagged = [file(f"g{i}", [GRID]) for i in range(reviewed + 1)]
        base = cr.summarize(run(*flagged))
        rc, out = check(run(*flagged), base)
        self.assertEqual(rc, 1, out)
        self.assertIn("reviewed_known_flags", out)

    def test_4_userunit_page_size_is_exempt(self):
        self.assertEqual(self.q(USERUNIT), "exempt:page-size")
        # 1-px rounding is compared normally.
        self.assertEqual(self.q(page(0.1, 0.1, sizes=(593, 792, 594, 792))), "ok")

    def test_5_blank_reference_is_exempt(self):
        self.assertEqual(self.q(JBIG2_BLANK), "exempt:reference-blank")

    def test_6_near_empty_page_is_not_evaluated(self):
        self.assertEqual(self.q(NEAR_EMPTY), "ok")
        self.assertEqual(self.q(page(0.0, 0.0)), "ok")

    def test_7_new_silent_misrender_fails_check(self):
        base = cr.summarize(run(file("a", [FIXED]), file("b", [NORMAL])))
        rc, out = check(run(file("a", [GARBLED]), file("b", [NORMAL])), base)
        self.assertEqual(rc, 1, out)
        self.assertIn("REGRESSION: a: exact render flag:ink_ratio", out)
        # A flagged file the baseline does not know fails too.
        rc, out = check(run(file("a", [FIXED]), file("b", [NORMAL]), file("c", [GARBLED])), base)
        self.assertEqual(rc, 1, out)
        # A fix is an improvement, never a failure.
        base = cr.summarize(run(file("a", [GARBLED])))
        rc, out = check(run(file("a", [FIXED])), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("improved: a: quality flag:ink_ratio -> ok", out)

    def test_report_only_rules_never_fail(self):
        soft = page(0.1, 0.22)  # ratio 0.45: inside the enforced range only
        self.assertEqual(self.q(soft), "report:ink_ratio_band")
        low_ssim = page(0.1, 0.1, cssim=0.2, diff=0.2)
        self.assertEqual(self.q(low_ssim), "report:content_ssim")
        base = cr.summarize(run(file("a", [NORMAL])))
        self.assertEqual(check(run(file("a", [soft, low_ssim])), base)[0], 0)

    def test_only_exact_renders_are_judged(self):
        self.assertEqual(self.q(GARBLED, approximate=True), "n/a")
        recovered = file("r", [GARBLED])
        recovered["recovered"] = True
        self.assertEqual(cr.quality(recovered, self.gate), "n/a")

    def test_baseline_without_gate_is_not_enforced(self):
        base = cr.summarize(run(file("a", [NORMAL])))
        for v in base["files"].values():
            for k in ("quality", "quality_pages", "quality_pages_flagged"):
                v.pop(k)
        rc, out = check(run(file("a", [GARBLED])), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("not enforced", out)

    def test_summary_markdown_counts_only(self):
        base = cr.summarize(run(file("grid", [GRID]), file("a", [FIXED])))
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "summary.md")
            rc, _ = check(run(file("grid", [GRID]), file("a", [GARBLED, NORMAL])), base, path)
            text = open(path).read()
        self.assertEqual(rc, 1)
        self.assertIn("quality gate: **FAIL**", text)
        self.assertIn("| pages compared | 3 |", text)
        self.assertIn("| new flags (fail) | 1 |", text)
        self.assertIn("| known flags (in the baseline) | 1 |", text)
        self.assertNotIn("grid", text)


if __name__ == "__main__":
    unittest.main()
