#!/usr/bin/env python3
"""Tests for the compatibility evaluation of M1 (docs/compat-evaluation.md):
the numeric comparison of scripts/ci-regression.py (M1-c) and the second
witness with its 300 dpi re-check, scripts/compat-attribute.py (M1-a,
M1-b). No corpus: synthetic report records, and fake renderers that
draw a known picture (or fail, hang, draw nothing, draw it at another
size) so that every verdict is known in advance.

    python3 scripts/test_compat_evaluation.py
    python3 scripts/test_compat_evaluation.py --real KASANE   (Full Gate)

--real runs the attribution with the installed Poppler, MuPDF and qpdf
on committed fixtures (tests/fixtures/), which every renderer draws
alike: each page must come out within tolerance of both witnesses, the
environment must carry both versions, and a fixture page beyond the
document's end must be not comparable.

The renderer tests need Pillow; they are skipped without it unless
KASANE_REQUIRE_PILLOW=1 (CI sets it, so a missing Pillow fails).
"""
import contextlib
import importlib.util
import io
import json
import os
import stat
import sys
import tempfile
import textwrap
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)


def load(name):
    spec = importlib.util.spec_from_file_location(name.replace("-", "_"), os.path.join(HERE, name + ".py"))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


cr = load("ci-regression")
ca = load("compat-attribute")

try:
    import PIL  # noqa: F401
    HAVE_PIL = True
except ImportError:
    HAVE_PIL = False
    if os.environ.get("KASANE_REQUIRE_PILLOW") == "1":
        raise

E0 = {"profile": "system", "version": 1, "config_sha256": "0" * 64, "fonts_sha256": "1" * 64, "font_files": 53,
      "fontconfig": "2.17.1", "freetype": "libfreetype.so.6.20.5"}
ENV = {"commit": "c" * 40, "os": "linux-ubuntu-26.04", "arch": "x86_64", "features": "cjk-fallback",
       "poppler": "26.01.0", "mupdf": "1.27.0", "qpdf": "12.3.2", "reference": E0}
TOL = dict(ca.TOLERANCE)


def page(n, ssim, cssim=0.9):
    return {"page": n, "ssim": ssim, "mae": 0.01, "diff_px_ratio": 0.01,
            "quality": {"content_ssim": cssim, "ink_ours": 0.1, "ink_ref": 0.1, "sizes": [100, 100, 100, 100],
                        "diff_px_at_best_shift": 0.0}}


def file(fid, pages, f1=1.0, render="pass"):
    return {"id": fid, "expect": "pass", "verdict": "pass" if render == "pass" else "error",
            "parse": {"class": "pass"}, "render": {"class": render}, "text": {"class": "pass"},
            "rewrite": {"class": "pass"}, "recovered": False, "render_approximate": True,
            "metrics": {"render_pages": pages, "text_char_f1": f1}}


def run(*files):
    return {"files": list(files)}


FAR = (0.05, 10.0)


def r300(ag, d, m):
    """Measures that give `ag`: (d, m) from the witness(es) it names
    (from both for `neither`), far from the other; references agree."""
    if ag == "not_comparable":
        return None
    kp = (d, m) if ag in ("both", "poppler", "neither") else FAR
    km = (d, m) if ag in ("both", "mupdf", "neither") else FAR
    return {"dpx_kp": kp[0], "mad_kp": kp[1], "dpx_km": km[0], "mad_km": km[1], "dpx_pm": 0.0, "mad_pm": 0.0}


def att(*rows, env=ENV, threshold=0.95, cap=None, truncated=None):
    pages = []
    for fid, pg, ag, d, m in rows:
        r = r300(ag, d, m)
        got, refs = ca.verdict(r)
        assert got == ag, (ag, d, m, got)
        pages.append({"id": fid, "page": pg, "ssim": 0.9, "class": "kasane", "agreement": ag,
                      "references_agree": refs, "reason": "failed:mupdf" if ag == "not_comparable" else None,
                      "r300": r})
    out = {"schema": 2, "env": env, "threshold": threshold, "tolerance": TOL, "max_pages_per_file": cap,
           "pages": pages}
    if truncated is not None:
        out["truncated"] = truncated
    return out


def check(r, base, **kw):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf):
        rc = cr.check(r, base, **kw)
    return rc, buf.getvalue()


def main_cli(*argv):
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(io.StringIO()):
        rc = cr.main(["ci-regression.py", *argv])
    return rc, buf.getvalue()


class NumericRegressions(unittest.TestCase):
    """M1-c: regressions inside a class, and what must never pass."""

    def base(self, *files, a=None):
        return cr.summarize(run(*files), a, ENV)

    def test_identical_run_passes(self):
        f = file("a", [page(1, 0.97), page(2, 0.9)])
        a = att(("a", 2, "poppler", 0.001, 0.5))
        rc, out = check(run(f), self.base(f, a=a), env=ENV, attribution=a, require_same_env=True)
        self.assertEqual(rc, 0, out)

    def test_ssim_drop_in_the_same_quality_class_is_a_regression(self):
        b = self.base(file("a", [page(1, 0.99)]))
        rc, out = check(run(file("a", [page(1, 0.985)])), b, env=ENV)
        self.assertEqual(rc, 1, out)
        self.assertIn("page 1 SSIM 0.99 -> 0.985", out)
        # Within the threshold: not a regression.
        rc, out = check(run(file("a", [page(1, 0.9895)])), b, env=ENV)
        self.assertEqual(rc, 0, out)

    def test_content_ssim_drop_is_a_regression(self):
        b = self.base(file("a", [page(1, 0.99, cssim=0.9)]))
        rc, out = check(run(file("a", [page(1, 0.99, cssim=0.85)])), b, env=ENV)
        self.assertEqual(rc, 1, out)
        self.assertIn("content SSIM 0.9 -> 0.85", out)

    def test_text_f1_drop_and_loss_are_regressions(self):
        b = self.base(file("a", [page(1, 0.99)], f1=1.0))
        rc, out = check(run(file("a", [page(1, 0.99)], f1=0.97)), b, env=ENV)
        self.assertEqual(rc, 1, out)
        self.assertIn("text_f1 1.0 -> 0.97", out)
        # A measure that disappears is not 0 and not a pass.
        rc, out = check(run(file("a", [page(1, 0.99)], f1=None)), b, env=ENV)
        self.assertEqual(rc, 1, out)
        self.assertIn("text_f1 no longer measured", out)

    def test_an_improvement_never_offsets_a_regression(self):
        b = self.base(file("a", [page(1, 0.90)]), file("b", [page(1, 0.99)]))
        rc, out = check(run(file("a", [page(1, 0.99)]), file("b", [page(1, 0.90)])), b, env=ENV)
        self.assertEqual(rc, 1, out)
        self.assertIn("improved: a: page 1 SSIM", out)
        self.assertIn("REGRESSION: b: page 1 SSIM", out)

    def test_a_page_no_longer_compared_is_a_regression(self):
        b = self.base(file("a", [page(1, 0.99), page(2, 0.99)]))
        rc, out = check(run(file("a", [page(1, 0.99)])), b, env=ENV)
        self.assertEqual(rc, 1, out)
        self.assertIn("page 2 no longer compared", out)

    def test_a_removed_case_is_a_regression(self):
        b = self.base(file("a", [page(1, 0.99)]), file("b", [page(1, 0.99)]))
        rc, out = check(run(file("a", [page(1, 0.99)])), b, env=ENV)
        self.assertEqual(rc, 1, out)
        self.assertIn("b: in the baseline, missing from the run", out)

    def test_environment(self):
        b = self.base(file("a", [page(1, 0.99)]))
        other = dict(ENV, poppler="26.10.0")
        worse = run(file("a", [page(1, 0.5)]))
        # Another witness version: the numeric measures are not compared
        # (never read as a pass of them), and the nightly fails.
        rc, out = check(worse, b, env=other)
        self.assertEqual(rc, 0, out)
        self.assertIn("numeric measures not compared: the environment differs", out)
        self.assertIn("poppler (26.01.0 -> 26.10.0)", out)
        rc, out = check(worse, b, env=other, require_same_env=True)
        self.assertEqual(rc, 1, out)
        rc, out = check(worse, b, require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("environment was not given", out)
        # A baseline without numeric measures says so.
        rc, out = check(worse, cr.summarize(run(file("a", [page(1, 0.99)]))), env=ENV)
        self.assertIn("the baseline records none", out)

    def test_attribution_moves(self):
        f = file("a", [page(1, 0.9), page(2, 0.9), page(3, 0.9)])
        a0 = att(("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", 0.02, 3.0), ("a", 3, "neither", 0.02, 3.0))
        b = self.base(f, a=a0)
        cases = [
            # (new rows, regression text or None)
            ([("a", 1, "neither", 0.02, 3.0), ("a", 2, "neither", 0.02, 3.0), ("a", 3, "neither", 0.02, 3.0)],
             "page 1 agreement both -> neither"),
            ([("a", 1, "both", 0.0, 0.1), ("a", 2, "not_comparable", 0, 0), ("a", 3, "neither", 0.02, 3.0)],
             "an open page hidden"),
            ([("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", 0.03, 3.0), ("a", 3, "neither", 0.02, 3.0)],
             "open page 2 differs more"),
            ([("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", 0.02, 3.6), ("a", 3, "neither", 0.02, 3.0)],
             "open page 2 differs more"),
            ([("a", 1, "both", 0.0, 0.1), ("a", 3, "neither", 0.02, 3.0)], "page 2 not attributed"),
            ([("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", 0.021, 3.1), ("a", 3, "poppler", 0.001, 0.1)], None),
        ]
        for rows, want in cases:
            rc, out = check(run(f), b, env=ENV, attribution=att(*rows), require_same_env=True)
            if want is None:
                self.assertEqual(rc, 0, out)
                self.assertIn("improved: a: page 3 agreement neither -> poppler", out)
            else:
                self.assertEqual(rc, 1, out)
                self.assertIn(want, out)

    def test_new_open_or_incomparable_pages_are_regressions(self):
        b = self.base(file("a", [page(1, 0.99)]), a=att())
        for ag in ("neither", "not_comparable"):
            rc, out = check(run(file("a", [page(1, 0.99)]), file("n", [page(1, 0.9)])), b, env=ENV,
                            attribution=att(("n", 1, ag, 0.02, 3.0)), require_same_env=True)
            self.assertEqual(rc, 1, out)
            self.assertIn(f"n: page 1 {ag} (file not in the baseline)", out)

    def test_a_newly_open_page_of_a_known_file_is_a_regression(self):
        f = file("a", [page(1, 0.9), page(2, 0.99)])
        b = self.base(f, a=att(("a", 1, "both", 0.0, 0.1)))
        g = file("a", [page(1, 0.9), page(2, 0.9)])
        rc, out = check(run(g), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", 0.02, 3)),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 2 newly neither", out)

    def test_attribution_required_and_consistent(self):
        f = file("a", [page(1, 0.9)])
        b = self.base(f, a=att(("a", 1, "both", 0.0, 0.1)))
        rc, out = check(run(f), b, env=ENV, require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("attribution not measured", out)
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), env=dict(ENV, mupdf="9")),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("another environment", out)
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), threshold=0.9),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("attribution threshold differs", out)
        # The witness results of another commit do not go with this run.
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), env=dict(ENV, commit="d" * 40)),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("another environment or commit", out)

    def test_an_unarmed_baseline_fails_the_nightly(self):
        b = cr.summarize(run(file("a", [page(1, 0.99)])))
        rc, out = check(run(file("a", [page(1, 0.1)])), b, env=ENV, attribution=att(("a", 1, "neither", 0.2, 50)),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("the baseline records none", out)

    def test_every_page_below_the_threshold_must_be_attributed(self):
        # A page that crosses the threshold by less than the SSIM
        # threshold needs its witness measurement all the same.
        b = self.base(file("a", [page(1, 0.9505)]), a=att())
        rc, out = check(run(file("a", [page(1, 0.9495)])), b, env=ENV, attribution=att(), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 1 below the attribution threshold but not attributed", out)
        # Attributing a page the run does not have below it is as wrong.
        rc, out = check(run(file("a", [page(1, 0.9505)])), b, env=ENV, attribution=att(("a", 1, "both", 0, 0)),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("attributed but not among", out)

    def test_one_witness_growing_is_a_regression(self):
        f = file("a", [page(1, 0.9)])
        b = self.base(f, a=att(("a", 1, "neither", 0.02, 3.0)))
        a = att(("a", 1, "neither", 0.02, 3.0))
        a["pages"][0]["r300"]["mad_kp"] = 20.0  # Poppler side much worse, MuPDF side unchanged
        rc, out = check(run(f), b, env=ENV, attribution=a, require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("mad_kp 3.0 -> 20.0", out)



# The Q1 situation, invented values: 14 pages below the threshold, a fix
# raises 12 of them by about 0.001, unevenly, and the 8 worst change —
# page 8 leaves them, page 10 enters them — though no page got worse.
Q1_BEFORE = [0.9311, 0.9312, 0.9326, 0.9318, 0.9316, 0.9305, 0.9330, 0.9317, 0.93175, 0.93177, 0.9331,
             0.9333, 0.9201, 0.9255]
Q1_AFTER = [0.9321, 0.9322, 0.9336, 0.9328, 0.9326, 0.9315, 0.9340, 0.9330, 0.93185, 0.93179, 0.9341,
            0.9343, 0.9201, 0.9255]
OPEN = (0.02, 3.0)  # outside both witnesses' tolerance


def worst(ssims, cap):
    return sorted(range(1, len(ssims) + 1), key=lambda pg: (ssims[pg - 1], pg))[:cap]


class PageSets(unittest.TestCase):
    """M2: which pages are measured is decided per (case, page), never by
    rank — a page that only changed rank is not a regression, a real
    change is."""

    def q1(self, ssims, cap, open_=OPEN):
        f = file("sf", [page(i + 1, s) for i, s in enumerate(ssims)])
        pages = list(range(1, len(ssims) + 1)) if cap is None else worst(ssims, cap)
        return f, att(*[("sf", pg, "neither", *open_) for pg in pages], cap=cap)

    def base(self, f, a):
        return cr.summarize(run(f), a, ENV)

    def test_q1_rank_swap_is_not_a_regression(self):
        before, a0 = self.q1(Q1_BEFORE, 8)
        after, a1 = self.q1(Q1_AFTER, 8)
        self.assertEqual(set(worst(Q1_BEFORE, 8)) - set(worst(Q1_AFTER, 8)), {8})
        self.assertEqual(set(worst(Q1_AFTER, 8)) - set(worst(Q1_BEFORE, 8)), {10})
        self.assertTrue(all(b >= a for a, b in zip(Q1_BEFORE, Q1_AFTER)), "no page got worse")
        # Both sides attribute every page below the threshold (M2): the
        # ranks no longer decide what is measured, and nothing fails.
        _, b_all = self.q1(Q1_BEFORE, None)
        _, all_ = self.q1(Q1_AFTER, None)
        rc, out = check(run(after), self.base(before, b_all), env=ENV, attribution=all_, require_same_env=True)
        self.assertEqual(rc, 0, out)
        self.assertNotIn("newly", out)
        self.assertNotIn("not attributed", out)
        # The M1 selection (8 worst) is refused outright: a page beyond
        # it would be below the threshold and never judged.
        old_base = self.base(before, a0)
        rc, out = check(run(after), old_base, env=ENV, attribution=a1, require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("attribution capped at 8 page(s) per file", out)
        # Every page against an M1 baseline (the migration): the pages the
        # old cap left out are unverified until the baseline attributes
        # them; the baseline's own targets are compared as usual.
        rc, out = check(run(after), old_base, env=ENV, attribution=all_, require_same_env=True)
        self.assertEqual(rc, 1, out)
        unverified = sorted(set(range(1, 15)) - set(worst(Q1_BEFORE, 8)))
        for pg in unverified:
            self.assertIn(f"sf: page {pg} first attributed: neither — unverified", out)
        self.assertEqual(out.count("unverified"), len(unverified))
        self.assertNotIn("newly", out)

    def test_a_first_measurement_is_never_a_pass(self):
        # A page below the threshold beyond an M1 baseline's cap whose
        # witness result now is anything short of both: unknown before,
        # so it cannot pass — it might have been both and lost one.
        f = file("a", [page(1, 0.9), page(2, 0.91)])
        b = self.base(f, att(("a", 1, "both", 0.0, 0.1), cap=1))
        for ag in ("neither", "not_comparable", "poppler", "mupdf"):
            m = (0.0, 0.1) if ag in ("poppler", "mupdf") else OPEN
            rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), ("a", 2, ag, *m),
                                                                cap=None), require_same_env=True)
            self.assertEqual(rc, 1, (ag, out))
            self.assertIn(f"a: page 2 first attributed: {ag} — unverified", out)
        # Within both: nothing can have been lost.
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), ("a", 2, "both", 0.0, 0.1),
                                                            cap=None), require_same_env=True)
        self.assertEqual(rc, 0, out)
        self.assertIn("a: page 2 newly attributed: both", out)

    def test_a_page_that_worsened_is_a_regression_whatever_its_rank(self):
        before, b = self.q1(Q1_BEFORE, None)
        worse = list(Q1_BEFORE)
        worse[9] -= 0.003  # page 10, now the 3rd worst
        after, a = self.q1(worse, None)
        rc, out = check(run(after), self.base(before, b), env=ENV, attribution=a, require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("sf: page 10 SSIM 0.9318 -> 0.9288", out)
        # Its distance to a witness growing is caught on its own as well.
        a2 = att(*[("sf", pg, "neither", *(OPEN if pg != 10 else (0.03, 3.0))) for pg in range(1, 15)], cap=None)
        rc, out = check(run(before), self.base(before, b), env=ENV, attribution=a2, require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("open page 10 differs more from Poppler", out)
        self.assertIn("open page 10 differs more from MuPDF", out)

    def test_pages_in_another_order_change_nothing(self):
        f = file("a", [page(1, 0.9), page(2, 0.8), page(3, 0.99)])
        a = att(("a", 1, "poppler", 0.001, 0.5), ("a", 2, "neither", *OPEN), cap=None)
        b = cr.summarize(run(f), a, ENV)
        g = file("a", [page(3, 0.99), page(2, 0.8), page(1, 0.9)])
        a2 = att(("a", 2, "neither", *OPEN), ("a", 1, "poppler", 0.001, 0.5), cap=None)
        rc, out = check(run(g), b, env=ENV, attribution=a2, require_same_env=True)
        self.assertEqual(rc, 0, out)

    def test_a_new_low_page_is_measured_and_judged(self):
        f = file("a", [page(1, 0.9), page(2, 0.9505)])
        b = cr.summarize(run(f), att(("a", 1, "both", 0.0, 0.1), cap=None), ENV)
        g = file("a", [page(1, 0.9), page(2, 0.9495)])  # drops by less than the SSIM threshold
        # Not measured: incomplete, never a pass.
        rc, out = check(run(g), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), cap=None),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 2 below the attribution threshold but not attributed", out)
        # Measured, outside both witnesses: a regression ...
        rc, out = check(run(g), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", *OPEN),
                                                             cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 2 newly neither (below the threshold, not in the baseline: SSIM 0.9505 -> 0.9495)", out)
        # ... labelled when the witnesses agree and kasane alone differs.
        self.assertIn("the references agree, kasane alone differs", out)
        # Within a witness: recorded, not failed.
        rc, out = check(run(g), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), ("a", 2, "mupdf", 0.0, 0.1),
                                                             cap=None), require_same_env=True)
        self.assertEqual(rc, 0, out)
        self.assertIn("a: page 2 newly attributed: mupdf", out)

    def test_a_page_the_baseline_does_not_have(self):
        f = file("a", [page(1, 0.9)])
        b = cr.summarize(run(f), att(("a", 1, "both", 0.0, 0.1), cap=None), ENV)
        g = file("a", [page(1, 0.9), page(2, 0.5)])
        rc, out = check(run(g), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", *OPEN),
                                                             cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 2 newly neither (below the threshold, not in the baseline: a page the baseline "
                      "does not have)", out)

    def test_deleted_pages_and_cases(self):
        f = file("a", [page(1, 0.9), page(2, 0.9)])
        b = cr.summarize(run(f, file("b", [page(1, 0.99)])),
                         att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN), cap=None), ENV)
        # A page gone: its measures are lost.
        rc, out = check(run(file("a", [page(1, 0.9)]), file("b", [page(1, 0.99)])), b, env=ENV,
                        attribution=att(("a", 1, "neither", *OPEN), cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 2 no longer compared", out)
        # A case gone.
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN),
                                                            cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("b: in the baseline, missing from the run", out)

    def test_a_baseline_target_without_its_measurement(self):
        f = file("a", [page(1, 0.9), page(2, 0.9)])
        b = cr.summarize(run(f), att(("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", *OPEN), cap=None), ENV)
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), cap=None),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 2 not attributed (below the attribution threshold; the baseline attributes it)", out)
        # One that left the threshold is an improvement, not a loss.
        g = file("a", [page(1, 0.9), page(2, 0.99)])
        rc, out = check(run(g), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), cap=None),
                        require_same_env=True)
        self.assertEqual(rc, 0, out)
        self.assertIn("a: page 2 no longer below the attribution threshold", out)

    def test_each_witness_on_its_own(self):
        f = file("a", [page(1, 0.9), page(2, 0.9)])
        b = cr.summarize(run(f), att(("a", 1, "both", 0.0, 0.1), ("a", 2, "neither", *OPEN), cap=None), ENV)
        for now, lost in (("mupdf", "Poppler"), ("poppler", "MuPDF"), ("neither", "Poppler and MuPDF")):
            rows = [("a", 1, now, *(OPEN if now == "neither" else (0.0, 0.1))), ("a", 2, "neither", *OPEN)]
            rc, out = check(run(f), b, env=ENV, attribution=att(*rows, cap=None), require_same_env=True)
            self.assertEqual(rc, 1, (now, out))
            self.assertIn(f"a: page 1 agreement both -> {now} (outside {lost}'s tolerance now", out)
        # An improvement elsewhere never offsets it.
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "mupdf", 0.0, 0.1), ("a", 2, "poppler", 0.0, 0.1),
                                                            cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("improved: a: page 2 agreement neither -> poppler", out)
        self.assertIn("REGRESSION: a: page 1 agreement both -> mupdf", out)
        # Not comparable is never an improvement.
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "not_comparable", 0, 0), ("a", 2, "neither", *OPEN),
                                                            cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 1 agreement both -> not_comparable", out)

    def test_coverage_and_truncation(self):
        f = file("a", [page(1, 0.9)])
        b = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), cap=None), ENV)
        # A run with a cap per file, whatever the baseline's.
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "neither", *OPEN), cap=8), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("attribution capped at 8 page(s) per file", out)
        # The same cap as the baseline's, a second low page beyond it.
        b1 = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), cap=1), ENV)
        g = file("a", [page(1, 0.9), page(2, 0.9)])
        rc, out = check(run(g), b1, env=ENV, attribution=att(("a", 1, "neither", *OPEN), cap=1), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("attribution capped at 1 page(s) per file", out)
        # A budget that left pages out: incomplete, never a pass.
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "neither", *OPEN), cap=None, truncated=3),
                        require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("attribution incomplete: 3 page(s)", out)
        # More pages than an older capped baseline: fine.
        b8 = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), cap=8), ENV)
        rc, out = check(run(f), b8, env=ENV, attribution=att(("a", 1, "neither", *OPEN), cap=None),
                        require_same_env=True)
        self.assertEqual(rc, 0, out)

    def test_a_baseline_that_should_have_attributed_a_page(self):
        f = file("a", [page(1, 0.9), page(2, 0.9)])
        # Below the threshold, under a cap that was not reached, yet not
        # attributed: the baseline cannot vouch for it.
        b = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), cap=8), ENV)
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN),
                                                            cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("the baseline is incomplete", out)
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "neither", *OPEN), ("a", 2, "both", 0, 0),
                                                            cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 2 both: below the threshold in the baseline", out)

    def test_rounding_and_boundaries(self):
        # SSIM drop of exactly 0.002 (recorded values): not a regression;
        # just over it: a regression.
        b = cr.summarize(run(file("a", [page(1, 0.99)])), None, ENV)
        self.assertEqual(check(run(file("a", [page(1, 0.988)])), b, env=ENV)[0], 0)
        rc, out = check(run(file("a", [page(1, 0.9879)])), b, env=ENV)
        self.assertEqual(rc, 1, out)
        # A page whose SSIM rounds to the threshold but is below it: the
        # baseline records that it was below, and it must be measured.
        f = file("a", [page(1, 0.94996)])
        b = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), cap=None), ENV)
        self.assertEqual(b["files"]["a"]["pages"]["1"], {"ssim": 0.95, "cssim": 0.9, "below": True})
        rc, out = check(run(f), b, env=ENV, attribution=att(cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 1 not attributed", out)
        # Beyond an older baseline's cap, the recorded flag (not the
        # rounded 0.95) says the page was below the threshold already: a
        # first measurement (unverified), not a new defect.
        f = file("a", [page(1, 0.9), page(2, 0.94996)])
        b = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), cap=1), ENV)
        rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN),
                                                            cap=None), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("a: page 2 first attributed: neither — unverified", out)
        self.assertNotIn("newly", out)
        # The witness measures at the tolerance: 0.005 / 2 are outside.
        self.assertEqual(ca.verdict({"dpx_kp": 0.005, "mad_kp": 0.0, "dpx_km": 0.0, "mad_km": 2.0,
                                     "dpx_pm": 0.0, "mad_pm": 0.0}), ("neither", True))
        self.assertEqual(ca.verdict({"dpx_kp": 0.00499, "mad_kp": 1.99, "dpx_km": 0.0, "mad_km": 0.0,
                                     "dpx_pm": 0.0, "mad_pm": 0.0}), ("both", True))
        # A negative SSIM is a value (-1..1), below -1 it is not.
        self.assertEqual(check(run(file("a", [page(1, -0.2)])), cr.summarize(run(file("a", [page(1, -0.2)])), None,
                                                                            ENV), env=ENV)[0], 0)

    def test_environment_mismatches(self):
        f = file("a", [page(1, 0.9)])
        b = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), cap=None), ENV)
        a = att(("a", 1, "neither", *OPEN), cap=None)
        for k, v in (("os", "macos-26"), ("arch", "arm64"), ("poppler", "26.10.0"), ("mupdf", "1.28.6"),
                     ("features", "")):
            rc, out = check(run(f), b, env=dict(ENV, **{k: v}), attribution=dict(a, env=dict(ENV, **{k: v})),
                            require_same_env=True)
            self.assertEqual(rc, 1, (k, out))
            self.assertIn("numeric measures not compared: the environment differs", out)
        rc, out = check(run(f), b, env=ENV, attribution=dict(a, env=dict(ENV, commit="d" * 40)), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("another environment or commit", out)
        # No commit on either side: the same commit cannot be established.
        nc = dict(ENV, commit=None)
        rc, out = check(run(f), b, env=nc, attribution=dict(a, env=nc), require_same_env=True)
        self.assertEqual(rc, 1, out)
        self.assertIn("environment incomplete: run commit, attribution commit", out)

    def test_an_unusable_attribution_is_not_judged_in_the_ledger(self):
        f = file("a", [page(1, 0.9)])
        b = cr.summarize(run(f), att(("a", 1, "both", 0, 0), cap=None), ENV)
        for a in (att(("a", 1, "neither", *OPEN), cap=None, env=dict(ENV, commit="d" * 40)),
                  att(("a", 1, "neither", *OPEN), cap=None, truncated=2)):
            with tempfile.TemporaryDirectory() as d:
                p = os.path.join(d, "l.json")
                rc, out = check(run(f), b, env=ENV, attribution=a, require_same_env=True, ledger_path=p)
                with open(p) as fh:
                    led = json.load(fh)
            self.assertEqual(rc, 1, out)
            self.assertEqual([(e["page"], e["reason"], e["decisions"]) for e in led], [(1, "attribution not usable", [])])

    def test_witness_measures_round_trip_at_the_tolerance(self):
        # Measures on both sides of each tolerance survive summarize ->
        # validate -> check unchanged in their verdicts.
        f = file("a", [page(1, 0.9), page(2, 0.9), page(3, 0.9), page(4, 0.9)])
        rows = [("a", 1, "both", 0.00499, 1.99999), ("a", 2, "neither", 0.005, 2.0),
                ("a", 3, "neither", 0.00499, 2.0), ("a", 4, "neither", 0.005, 1.99999)]
        a = att(*rows, cap=None)
        b = cr.summarize(run(f), a, ENV)
        cr.validate_base(json.loads(json.dumps(b)))
        self.assertEqual({pg: v["agreement"] for pg, v in b["files"]["a"]["attribution"].items()},
                         {"1": "both", "2": "neither", "3": "neither", "4": "neither"})
        rc, out = check(run(f), b, env=ENV, attribution=a, require_same_env=True)
        self.assertEqual(rc, 0, out)

    def test_the_ledger(self):
        f = file("a", [page(1, 0.9), page(2, 0.99), page(3, 0.9)])
        b = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), ("a", 3, "both", 0, 0), cap=None), ENV)
        g = file("a", [page(1, 0.9), page(2, 0.9), page(3, 0.99)])
        with tempfile.TemporaryDirectory() as d:
            p = os.path.join(d, "ledger.json")
            check(run(g), b, env=ENV, attribution=att(("a", 1, "neither", *OPEN), ("a", 2, "both", 0, 0), cap=None),
                  require_same_env=True, ledger_path=p)
            with open(p) as fh:
                led = {e["page"]: e for e in json.load(fh)}
        self.assertEqual(sorted(led), [1, 2, 3])
        self.assertEqual((led[1]["baseline_target"], led[1]["attributed"], led[1]["selected_because"]),
                         (True, True, "baseline target"))
        self.assertEqual((led[2]["new_candidate"], led[2]["selected_because"]), (True, "below the threshold"))
        self.assertEqual(led[3]["reason"], "no longer below the threshold")
        for e in led.values():
            for k in ("id", "page", "in_baseline", "in_run", "baseline_target", "new_candidate", "selected_because",
                      "comparable", "reason", "decisions"):
                self.assertIn(k, e)


class InvalidInputsM2(unittest.TestCase):
    def write(self, d, name, text):
        p = os.path.join(d, name)
        with open(p, "w") as fh:
            fh.write(text if isinstance(text, str) else json.dumps(text))
        return p

    def test_rejected(self):
        good = run(file("a", [page(1, 0.9)]))
        a = att(("a", 1, "neither", *OPEN), cap=None)
        base = cr.summarize(good, a, ENV)
        with tempfile.TemporaryDirectory() as d:
            r, b = self.write(d, "r.json", good), self.write(d, "b.json", base)
            at = self.write(d, "a.json", a)
            env = self.write(d, "e.json", ENV)
            ok = ("check", r, b, "--env", env, "--attribution", at, "--require-same-env")
            self.assertEqual(main_cli(*ok)[0], 0)
            bt = json.dumps(base)
            cases = {
                "a duplicate case id in the baseline (JSON keys)": ("check", r, self.write(
                    d, "dupk.json", bt.replace('"files": {', '"files": {"a": {"stages": {}}, ', 1))),
                "a duplicate key in the attribution": ("check", r, b, "--attribution", self.write(
                    d, "dupa.json", json.dumps(a).replace('"schema": 2', '"schema": 2, "schema": 2'))),
                "an SSIM below -1": ("check", self.write(d, "neg.json", run(file("a", [page(1, -1.5)]))), b),
                "an SSIM above 1": ("check", self.write(d, "big.json", run(file("a", [page(1, 1.5)]))), b),
                "a content SSIM out of range": ("check", self.write(d, "cs.json", run(file("a", [page(1, 0.9, 2.0)]))), b),
                "a text F1 above 1": ("check", self.write(d, "f1.json", run(file("a", [page(1, 0.9)], f1=1.5))), b),
                "a text F1 below 0": ("check", self.write(d, "f0.json", run(file("a", [page(1, 0.9)], f1=-0.1))), b),
                "page 0": ("check", self.write(d, "p0.json", run(file("a", [page(0, 0.9)]))), b),
                "a page without an SSIM, in a new case": ("check", self.write(d, "ns.json", run(
                    file("a", [page(1, 0.9)]), file("new", [page(1, None)]))), b),
                "a page record that is not a record": ("check", self.write(d, "nr.json", dict(run(
                    file("a", [page(1, 0.9)])), files=[dict(file("a", [page(1, 0.9)]), metrics={
                        "render_pages": [page(1, 0.9), 7]})])), b),
                "a page that is not a number": ("check", self.write(d, "pn.json", run(file("a", [page("1", 0.9)]))), b),
                "a baseline SSIM out of range": ("check", r, self.write(d, "bs.json", bt.replace(
                    '"ssim": 0.9', '"ssim": 9.0'))),
                "a baseline agreement its measures do not give": ("check", r, self.write(d, "ba.json", bt.replace(
                    '"agreement": "neither"', '"agreement": "both"'))),
                "a baseline agreement without measures": ("check", r, self.write(d, "bm.json", bt.replace(
                    '"dpx_kp": 0.02', '"dpx_kp": null'))),
                "a page cap of 0": ("check", r, b, "--attribution", self.write(d, "c0.json", dict(
                    a, max_pages_per_file=0))),
                "a negative truncation": ("check", r, b, "--attribution", self.write(d, "tn.json", dict(a, truncated=-1))),
                "a non-JSON attribution": ("check", r, b, "--attribution", self.write(d, "nj.json", "{not json")),
                "an empty attribution file": ("check", r, b, "--attribution", self.write(d, "em.json", "")),
            }
            for name, argv in cases.items():
                rc, out = main_cli(*argv)
                self.assertEqual(rc, 2, f"{name}: {out}")
                self.assertIn("INVALID", out, name)

    def test_update_baseline_never_records_an_incomplete_attribution(self):
        good = run(file("a", [page(1, 0.9), page(2, 0.9)]))
        with tempfile.TemporaryDirectory() as d:
            r, env = self.write(d, "r.json", good), self.write(d, "e.json", ENV)
            out_b = os.path.join(d, "b.json")
            for name, a in (("missing", att(("a", 1, "neither", *OPEN), cap=None)),
                            ("truncated", att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN), cap=None,
                                              truncated=1))):
                rc, out = main_cli("update-baseline", r, out_b, "--env", env, "--attribution",
                                   self.write(d, f"a-{name}.json", a), "--write")
                self.assertEqual(rc, 2, f"{name}: {out}")
                self.assertFalse(os.path.exists(out_b), name)
            capped = att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN), cap=8)
            rc, out = main_cli("update-baseline", r, out_b, "--env", env, "--attribution",
                               self.write(d, "a-capped.json", capped), "--write")
            self.assertEqual(rc, 2, out)
            self.assertIn("capped", out)
            self.assertFalse(os.path.exists(out_b))
            full = att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN), cap=None)
            rc, out = main_cli("update-baseline", r, out_b, "--env", env, "--attribution", self.write(d, "a.json", full))
            self.assertEqual(rc, 0, out)
            self.assertFalse(os.path.exists(out_b), "a dry run writes nothing")
            rc, out = main_cli("update-baseline", r, out_b, "--env", env, "--attribution",
                               self.write(d, "a.json", full), "--write")
            self.assertEqual(rc, 0, out)
            with open(out_b) as fh:
                self.assertIsNone(json.load(fh)["meta"]["attribution"]["max_pages_per_file"])

    def test_the_migration_accepts_first_measurements_and_nothing_else(self):
        with tempfile.TemporaryDirectory() as d:
            env = self.write(d, "e.json", ENV)
            f = file("a", [page(1, 0.9), page(2, 0.91)])
            old = cr.summarize(run(f), att(("a", 1, "neither", *OPEN), cap=1), ENV)  # an M1 baseline
            b = self.write(d, "b.json", old)
            r = self.write(d, "r.json", run(f))
            a = self.write(d, "a.json", att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN)))
            rc, out = main_cli("update-baseline", r, b, "--env", env, "--attribution", a, "--write")
            self.assertEqual(rc, 1, out)
            self.assertIn("1 page(s) measured for the first time are unverified", out)
            rc, out = main_cli("update-baseline", r, b, "--env", env, "--attribution", a, "--write",
                               "--accept-unverified")
            self.assertEqual(rc, 0, out)
            self.assertIn("accepted: 1 first measurement(s)", out)
            # With any other regression, --accept-unverified is not enough.
            self.write(d, "b.json", old)
            worse = self.write(d, "w.json", run(file("a", [page(1, 0.8), page(2, 0.91)])))
            rc, out = main_cli("update-baseline", worse, b, "--env", env, "--attribution", a, "--write",
                               "--accept-unverified")
            self.assertEqual(rc, 1, out)
            self.assertIn("regressions against the current baseline (1;", out)
            with open(b) as fh:
                self.assertEqual(json.load(fh), json.loads(json.dumps(old)))
            # The kind is the ledger's, not the text's: a case whose id
            # carries the mark gains nothing from it.
            mark = "x" + cr.UNVERIFIED
            g = file(mark, [page(1, 0.95)])
            old2 = cr.summarize(run(f, g), att(("a", 1, "neither", *OPEN), cap=1), ENV)
            self.write(d, "b.json", old2)
            r2 = self.write(d, "r2.json", run(f, file(mark, [page(1, 0.90)])))
            a2 = self.write(d, "a2.json", att(("a", 1, "neither", *OPEN), ("a", 2, "neither", *OPEN),
                                              (mark, 1, "neither", *OPEN)))
            rc, out = main_cli("update-baseline", r2, b, "--env", env, "--attribution", a2, "--write",
                               "--accept-unverified")
            self.assertEqual(rc, 1, out)
            self.assertIn("regressions against the current baseline (2;", out)
            # A first measurement that cannot be measured is not accepted
            # as a migration: it is a regression to decide on.
            self.write(d, "b.json", old)
            a3 = self.write(d, "a3.json", att(("a", 1, "neither", *OPEN), ("a", 2, "not_comparable", 0, 0)))
            rc, out = main_cli("update-baseline", r, b, "--env", env, "--attribution", a3, "--write",
                               "--accept-unverified")
            self.assertEqual(rc, 1, out)
            self.assertIn("regressions against the current baseline (1;", out)


class Selection(unittest.TestCase):
    """compat-attribute's page selection (no renderers)."""

    def test_every_page_below_the_threshold_and_a_budget(self):
        rep = run(file("a", [page(i, 0.9 - i / 100) for i in range(1, 13)]), file("b", [page(1, 0.99)]))
        items = ca.pages_to_check(rep, 0.95)
        self.assertEqual([pg for _, pg, _ in items], list(range(12, 0, -1)))
        self.assertEqual(len(ca.pages_to_check(rep, 0.95, 8)), 8)
        keep, cut = ca.within_budget(items, 5)
        self.assertEqual((len(keep), cut), (5, 7))
        self.assertEqual([pg for _, pg, _ in keep], [12, 11, 10, 9, 8])
        self.assertEqual(ca.within_budget(items, 100), (items, 0))

class InvalidInputs(unittest.TestCase):
    """A report or baseline that cannot be compared is rejected (2), never
    compared and never passed."""

    def write(self, d, name, obj):
        p = os.path.join(d, name)
        with open(p, "w") as fh:
            fh.write(obj if isinstance(obj, str) else json.dumps(obj))
        return p

    def test_rejected(self):
        good = run(file("a", [page(1, 0.99)]))
        base = cr.summarize(good, None, ENV)
        with tempfile.TemporaryDirectory() as d:
            r, b = self.write(d, "r.json", good), self.write(d, "b.json", base)
            self.assertEqual(main_cli("check", r, b)[0], 0)
            cases = {
                "missing baseline": ("check", r, os.path.join(d, "none.json")),
                "missing report (the runner died)": ("check", os.path.join(d, "none.json"), b),
                "truncated baseline": ("check", r, self.write(d, "t.json", json.dumps(base)[:200])),
                "baseline without files": ("check", r, self.write(d, "x.json", {"aggregate": {}})),
                "NaN in the report": ("check", self.write(d, "n.json", json.dumps(
                    run(file("a", [page(1, float("nan"))])))), b),
                "Infinity in the baseline": ("check", r, self.write(d, "i.json", json.dumps(base).replace(
                    '"ssim": 0.99', '"ssim": Infinity'))),
                "duplicate ids": ("check", self.write(d, "dup.json", run(file("a", []), file("a", []))), b),
                "page measures without an environment": ("check", r, self.write(d, "noenv.json", dict(
                    base, meta={}))),
                "attribution of another schema": ("check", r, b, "--attribution", self.write(
                    d, "a.json", {"schema": 1, "files": []})),
                "unknown agreement": ("check", r, b, "--attribution", self.write(
                    d, "a2.json", dict(att(("a", 1, "both", 0, 0)), pages=[dict(att(("a", 1, "both", 0, 0))[
                        "pages"][0], agreement="ok")]))),
                "a label its measures do not give": ("check", r, b, "--attribution", self.write(
                    d, "a3.json", dict(att(("a", 1, "neither", 0.2, 9)), pages=[dict(att(("a", 1, "neither", 0.2, 9))[
                        "pages"][0], agreement="both")]))),
                "agreement without measures": ("check", r, b, "--attribution", self.write(
                    d, "a4.json", dict(att(("a", 1, "both", 0, 0)), pages=[dict(att(("a", 1, "both", 0, 0))[
                        "pages"][0], r300=None)]))),
                "a measure out of range": ("check", r, b, "--attribution", self.write(
                    d, "a5.json", json.loads(json.dumps(att(("a", 1, "both", 0, 0))).replace(
                        '"dpx_pm": 0.0', '"dpx_pm": 7.0')))),
                "a page attributed twice": ("check", r, b, "--attribution", self.write(
                    d, "a6.json", att(("a", 1, "both", 0, 0), ("a", 1, "neither", 0.2, 9)))),
                "another tolerance": ("check", r, b, "--attribution", self.write(
                    d, "a7.json", dict(att(("a", 1, "both", 0, 0)), tolerance={"dpx": 1, "mad": 255}))),
                "a page measured twice in the report": ("check", self.write(d, "p2.json", run(
                    file("a", [page(1, 0.5), page(1, 0.99)]))), b),
            }
            for name, argv in cases.items():
                rc, out = main_cli(*argv)
                self.assertEqual(rc, 2, f"{name}: {out}")
                self.assertIn("INVALID", out, name)


ce = load("compat_env")


class ReferenceFingerprint(unittest.TestCase):
    """The reference renderer's fonts are part of the environment: E0 and
    E1 are never the same, a missing fingerprint never matches, and the
    same setup elsewhere on disk is the same."""

    E1 = dict(E0, profile="courier-nimbus-mono-ps", config_sha256="2" * 64, fonts_sha256="3" * 64, font_files=57)

    def test_differences(self):
        d = ce.differences
        self.assertEqual(d(ENV, dict(ENV)), [])
        self.assertEqual(d(ENV, dict(ENV, reference=self.E1)), ["reference"])
        # Missing on one side, on both, or incomplete: never the same.
        no_ref = {k: v for k, v in ENV.items() if k != "reference"}
        self.assertEqual(d(no_ref, ENV), ["reference"])
        self.assertEqual(d(ENV, no_ref), ["reference"])
        self.assertEqual(d(no_ref, dict(no_ref)), ["reference"])
        partial = dict(ENV, reference={k: v for k, v in E0.items() if k != "fonts_sha256"})
        self.assertEqual(d(partial, dict(partial)), ["reference"])
        self.assertEqual(d(ENV, dict(ENV, reference=dict(E0, config_sha256=None))), ["reference"])
        # Another architecture is another environment.
        self.assertEqual(d(ENV, dict(ENV, arch="aarch64")), ["arch"])
        self.assertIn("reference", ce.incomplete(no_ref, need_mupdf=True))
        self.assertNotIn("reference", ce.incomplete(ENV, need_mupdf=True))
        # Malformed or empty fields are incomplete, and so never equal.
        for bad in (dict(E0, font_files=0), dict(E0, font_files="53"), dict(E0, freetype=None),
                    {k: v for k, v in E0.items() if k != "freetype"}, dict(E0, config_sha256="<unreadable>"),
                    dict(E0, fonts_sha256="A" * 64), dict(E0, version=0), dict(E0, version="1"),
                    dict(E0, profile="unidentified"), dict(E0, profile="")):
            self.assertFalse(ce.reference_complete(bad), bad)
            self.assertEqual(d(dict(ENV, reference=bad), dict(ENV, reference=dict(bad))), ["reference"])
            self.assertIn("reference", ce.incomplete(dict(ENV, reference=bad), need_mupdf=True))

    def setup_tools(self, d):
        """Fake fc-conflist / fc-list reading a toy configuration: lines
        `include PATH` (the files loaded) and `font PATH` (the fonts)."""
        bin_ = os.path.join(d, "bin")
        os.makedirs(bin_)
        conflist = ('#!/bin/sh\nf=${FONTCONFIG_FILE:-/nonexistent}\n[ -f "$f" ] || exit 0\n'
                    'sed -n "s/^include //p" "$f" | while read -r p; do echo "+ $p: x"; done\n')
        fclist = ('#!/bin/sh\n[ "$1" = --version ] && { echo "fontconfig version 2.17.1"; exit 0; }\n'
                  'f=${FONTCONFIG_FILE:-/nonexistent}\n[ -f "$f" ] || exit 0\n'
                  'sed -n "s/^font //p" "$f"\n'
                  'for i in $(sed -n "s/^include //p" "$f"); do sed -n "s/^font //p" "$i"; done\n')
        ldd = '#!/bin/sh\necho "\tlibfreetype.so.6 => /lib/libfreetype.so.6.20.5 (0x0)"\n'
        for n, body in (("fc-conflist", conflist), ("fc-list", fclist), ("ldd", ldd)):
            with open(os.path.join(bin_, n), "w") as fh:
                fh.write(body)
            os.chmod(os.path.join(bin_, n), 0o755)
        return bin_

    def reference_dir(self, root, font=b"nimbus", include=b"rule A", profile=True):
        """A setup-e1.sh-like directory: fonts.conf, one font, an
        included rule file, profile.json, a pdftoppm wrapper."""
        os.makedirs(os.path.join(root, "fonts"))
        os.makedirs(os.path.join(root, "bin"))
        with open(os.path.join(root, "fonts", "f.otf"), "wb") as fh:
            fh.write(font)
        with open(os.path.join(root, "rules.conf"), "wb") as fh:
            fh.write(include)
        with open(os.path.join(root, "fonts.conf"), "w") as fh:
            fh.write(f"include {root}/rules.conf\nfont {root}/fonts/f.otf\n")
        if profile:
            with open(os.path.join(root, "profile.json"), "w") as fh:
                json.dump({"profile": "courier-nimbus-mono-ps", "version": 1}, fh)
        w = os.path.join(root, "bin", "pdftoppm")
        with open(w, "w") as fh:
            fh.write(self.wrapper(root))
        os.chmod(w, 0o755)
        return w

    BINARY = os.path.realpath(sys.executable)  # any executable binary stands for Poppler

    def wrapper(self, root, binary=None):
        """setup-e1.sh's wrapper, exactly."""
        return ("#!/bin/sh\nexec /usr/bin/env -u FONTCONFIG_PATH -u FONTCONFIG_SYSROOT "
                f"FONTCONFIG_FILE={root}/fonts.conf {binary or self.BINARY} \"$@\"\n")

    def fingerprint(self, wrapper, bin_, **env):
        saved = dict(os.environ)
        os.environ["PATH"] = bin_ + os.pathsep + saved.get("PATH", "")
        for k in ("FONTCONFIG_FILE", "FONTCONFIG_PATH", "FONTCONFIG_SYSROOT"):
            os.environ.pop(k, None)
        os.environ.update(env)
        try:
            return ce.reference(wrapper)
        finally:
            os.environ.clear()
            os.environ.update(saved)

    def test_reference_fails_closed(self):
        with tempfile.TemporaryDirectory() as d:
            bin_ = self.setup_tools(d)
            good = self.reference_dir(os.path.join(d, "good"))
            a = self.fingerprint(good, bin_)
            self.assertTrue(ce.reference_complete(a), a)
            # The wrapper clears FONTCONFIG_PATH / FONTCONFIG_SYSROOT, so the
            # fingerprint does not see them either.
            self.assertEqual(self.fingerprint(good, bin_, FONTCONFIG_PATH=d, FONTCONFIG_SYSROOT=d), a)
            root = os.path.join(d, "good")

            def tool(name, body, mode=0o755):
                p = os.path.join(d, name)
                with open(p, "w") as fh:
                    fh.write(body)
                os.chmod(p, mode)
                return p
            w = self.wrapper(root)
            unidentified = [
                # Another form, the same file named: not setup-e1.sh's.
                tool("other", f"#!/bin/sh\nFONTCONFIG_FILE={root}/fonts.conf exec {self.BINARY} \"$@\"\n"),
                # setup-e1.sh's, plus something else.
                tool("extra", w + "export FONTCONFIG_FILE=/elsewhere\n"),
                tool("prefix", "#!/bin/sh\nFONTCONFIG_FILE=/elsewhere; export FONTCONFIG_FILE\n" + w[len("#!/bin/sh\n"):]),
                # A wrapper of a wrapper.
                tool("nested", self.wrapper(root, binary=good)),
                # A script that names no configuration.
                tool("plain", f"#!/bin/sh\nexec {self.BINARY} \"$@\"\n"),
                os.path.join(d, "missing"),
            ]
            unreadable = tool("unreadable", w, mode=0o000)
            if not os.access(unreadable, os.R_OK):  # (root reads anything)
                unidentified.append(unreadable)
            for t in unidentified:
                r = self.fingerprint(t, bin_)
                self.assertEqual(r["profile"], "unidentified", t)
                self.assertFalse(ce.reference_complete(r), t)
            # A binary run with a variable the fingerprint does not follow.
            for k in ("FONTCONFIG_PATH", "FONTCONFIG_SYSROOT"):
                self.assertFalse(ce.reference_complete(self.fingerprint(self.BINARY, bin_, **{k: d})), k)
            # A listed font or included file that cannot be read, an empty
            # font list, an unknown FreeType: incomplete.
            for name, broken in (("font", os.path.join(root, "fonts", "f.otf")),
                                 ("include", os.path.join(root, "rules.conf"))):
                os.rename(broken, broken + ".gone")
                r = self.fingerprint(good, bin_)
                self.assertFalse(ce.reference_complete(r), name)
                os.rename(broken + ".gone", broken)
            with open(os.path.join(root, "fonts.conf"), "w") as fh:
                fh.write(f"include {root}/rules.conf\n")
            self.assertIsNone(self.fingerprint(good, bin_)["font_files"])
            os.remove(os.path.join(bin_, "ldd"))
            self.assertFalse(ce.reference_complete(self.fingerprint(good, bin_)))

    def test_reference_follows_content_not_place(self):
        with tempfile.TemporaryDirectory() as d:
            bin_ = self.setup_tools(d)
            a = self.fingerprint(self.reference_dir(os.path.join(d, "a")), bin_)
            b = self.fingerprint(self.reference_dir(os.path.join(d, "tmp", "elsewhere", "b")), bin_)
            self.assertTrue(ce.reference_complete(a), a)
            self.assertEqual(a, b)  # only the directory differs
            self.assertEqual((a["profile"], a["version"], a["font_files"]), ("courier-nimbus-mono-ps", 1, 1))
            font = self.fingerprint(self.reference_dir(os.path.join(d, "c"), font=b"another"), bin_)
            self.assertNotEqual(a["fonts_sha256"], font["fonts_sha256"])
            self.assertEqual(a["config_sha256"], font["config_sha256"])
            rule = self.fingerprint(self.reference_dir(os.path.join(d, "e"), include=b"rule B"), bin_)
            self.assertNotEqual(a["config_sha256"], rule["config_sha256"])
            # A configuration that does not declare its profile is not one.
            undeclared = self.fingerprint(self.reference_dir(os.path.join(d, "f"), profile=False), bin_)
            self.assertEqual(undeclared["profile"], "undeclared")
            self.assertNotEqual(a, undeclared)

    def test_corpus_and_attribution_in_other_references(self):
        f = file("a", [page(1, 0.9)])
        a = att(("a", 1, "both", 0.0, 0.1))
        b = cr.summarize(run(f), a, ENV)
        for other in (dict(ENV, reference=self.E1), {k: v for k, v in ENV.items() if k != "reference"}):
            rc, out = check(run(f), b, env=ENV, attribution=att(("a", 1, "both", 0.0, 0.1), env=other),
                            require_same_env=True)
            self.assertEqual(rc, 1, out)
            self.assertIn("attribution measured in another environment or commit than the run: reference", out)
            # The run itself in another reference than the baseline.
            rc, out = check(run(f), b, env=other, attribution=att(("a", 1, "both", 0.0, 0.1), env=other),
                            require_same_env=True)
            self.assertEqual(rc, 1, out)
            self.assertIn("reference", out)

    def test_update_baseline_needs_a_reference(self):
        with tempfile.TemporaryDirectory() as d:
            env = os.path.join(d, "env.json")
            with open(env, "w") as fh:
                json.dump({k: v for k, v in ENV.items() if k != "reference"}, fh)
            r = os.path.join(d, "r.json")
            with open(r, "w") as fh:
                json.dump(run(file("a", [page(1, 0.99)])), fh)
            rc, out = main_cli("update-baseline", r, os.path.join(d, "b.json"), "--env", env, "--write")
            self.assertNotEqual(rc, 0, out)
            self.assertIn("reference", out)
            self.assertFalse(os.path.exists(os.path.join(d, "b.json")))
            # An incomplete reference: refused too, and a strict check of a
            # run with one against a baseline with the same fails.
            bad = dict(E0, font_files=0, freetype=None)
            with open(env, "w") as fh:
                json.dump(dict(ENV, reference=bad), fh)
            rc, out = main_cli("update-baseline", r, os.path.join(d, "b.json"), "--env", env, "--write",
                               "--accept-regressions")
            self.assertNotEqual(rc, 0, out)
            self.assertFalse(os.path.exists(os.path.join(d, "b.json")))
            f = file("a", [page(1, 0.9)])
            a = att(("a", 1, "both", 0.0, 0.1), env=dict(ENV, reference=bad))
            b = cr.summarize(run(f), a, dict(ENV, reference=bad))
            rc, out = check(run(f), b, env=dict(ENV, reference=bad), attribution=a, require_same_env=True)
            self.assertEqual(rc, 1, out)
            self.assertIn("reference", out)

class UpdateBaseline(unittest.TestCase):
    def test_explicit_and_protected(self):
        with tempfile.TemporaryDirectory() as d:
            def w(n, o):
                with open(os.path.join(d, n), "w") as fh:
                    json.dump(o, fh)
                return os.path.join(d, n)

            def read(p):
                with open(p) as fh:
                    return fh.read()
            env = w("env.json", ENV)
            r1 = w("r1.json", run(file("a", [page(1, 0.99)]), file("b", [page(1, 0.99)])))
            b = os.path.join(d, "base.json")
            # Dry run: nothing written.
            rc, out = main_cli("update-baseline", r1, b, "--env", env)
            self.assertEqual(rc, 0, out)
            self.assertFalse(os.path.exists(b))
            self.assertIn("dry run", out)
            rc, out = main_cli("update-baseline", r1, b, "--env", env, "--write")
            self.assertEqual(rc, 0, out)
            first = read(b)
            self.assertEqual(json.loads(first)["meta"]["env"], ENV)
            # Removing a case, or accepting a regression, needs a flag;
            # the diff is shown either way.
            r2 = w("r2.json", run(file("a", [page(1, 0.95)])))
            rc, out = main_cli("update-baseline", r2, b, "--env", env, "--write")
            self.assertEqual(rc, 1, out)
            self.assertIn("removed files: 1", out)
            self.assertIn("page SSIM lower: 1", out)
            self.assertEqual(read(b), first)
            rc, out = main_cli("update-baseline", r2, b, "--env", env, "--write", "--allow-removed")
            self.assertEqual(rc, 1, out)
            self.assertIn("regressions against the current baseline", out)
            self.assertEqual(read(b), first)
            rc, out = main_cli("update-baseline", r2, b, "--env", env, "--write", "--allow-removed",
                               "--accept-regressions")
            self.assertEqual(rc, 0, out)
            # An incomplete environment is never recorded.
            rc, out = main_cli("update-baseline", r1, b, "--env", w("e2.json", dict(ENV, poppler=None)), "--write")
            self.assertEqual(rc, 2, out)
            self.assertIn("environment incomplete", out)
            # The default output names no file.
            rc, out = main_cli("update-baseline", r1, b, "--env", env)
            self.assertNotIn("[b]", out)
            rc, out = main_cli("update-baseline", r1, b, "--env", env, "--ids")
            self.assertIn("[b]", out)


class Decisions(unittest.TestCase):
    """compat-attribute's verdicts from measures (no images)."""

    def r(self, kp=(0.0, 0.0), km=(0.0, 0.0), pm=(0.0, 0.0)):
        return {"dpx_kp": kp[0], "mad_kp": kp[1], "dpx_km": km[0], "mad_km": km[1], "dpx_pm": pm[0], "mad_pm": pm[1]}

    def test_verdicts(self):
        far = (0.02, 5.0)
        self.assertEqual(ca.verdict(self.r()), ("both", True))
        self.assertEqual(ca.verdict(self.r(km=far, pm=far)), ("poppler", False))
        self.assertEqual(ca.verdict(self.r(kp=far, pm=far)), ("mupdf", False))
        self.assertEqual(ca.verdict(self.r(kp=far, km=far)), ("neither", True))
        self.assertEqual(ca.verdict(self.r(kp=far, km=far, pm=far)), ("neither", False))
        # Local differences with an almost untouched mean are not agreement.
        self.assertEqual(ca.verdict(self.r(kp=(0.01, 0.5), km=(0.01, 0.5)))[0], "neither")
        self.assertEqual(ca.verdict(self.r(kp=(0.0049, 1.99)))[0], "both")
        # A broad tone difference (no pixel past 48 levels) is not agreement.
        self.assertEqual(ca.verdict(self.r(kp=(0.0, 3.0), km=(0.0, 3.0)))[0], "neither")
        # Missing or non-finite measures: not comparable, never agreement.
        for bad in (None, {}, dict(self.r(), dpx_kp=None), dict(self.r(), mad_km=float("nan")),
                    dict(self.r(), dpx_pm=float("inf")), dict(self.r(), dpx_kp=True)):
            self.assertEqual(ca.verdict(bad), ("not_comparable", None), bad)

    def test_a_produced_record_validates_at_the_tolerance_boundary(self):
        # Measures just under the tolerance, rounded on output: the
        # record compat-attribute writes is the one the gate accepts.
        orig, orig_path = ca.measure_page, ca.corpus_path
        try:
            for d in (0.004999, 0.0049999, 0.005, 0.0050001):
                ca.measure_page = lambda *a, d=d: ({}, {"dpx_kp": d, "dpx_km": d, "dpx_pm": 0.0,
                                                       "mad_kp": 1.0, "mad_km": 1.999996, "mad_pm": 0.0}, None)
                ca.corpus_path = lambda fid: "x.pdf"
                row = ca.assess(({"id": "a"}, 1, 0.9), "kasane", 1)
                doc = {"schema": 2, "env": ENV, "threshold": 0.95, "tolerance": TOL, "max_pages_per_file": 8,
                       "pages": [row]}
                cr.validate_attribution(doc)
        finally:
            ca.measure_page, ca.corpus_path = orig, orig_path

    def test_kasane_defects_come_before_reference_disagreement(self):
        m = {"ink_kasane": 0.01, "ink_poppler": 0.2, "ink_mupdf": 0.2, "dpx_kp": 0.3, "dpx_km": 0.01, "dpx_pm": 0.5}
        self.assertEqual(ca.classify72(m), "blank")
        m.update(ink_kasane=0.1)
        self.assertEqual(ca.classify72(m), "missing_ink")
        m.update(ink_kasane=0.2)
        self.assertEqual(ca.classify72(m), "reference_kasane_mupdf")
        self.assertEqual(ca.classify72({}), "not_comparable")


FAKE = r'''#!{python}
import json, os, sys, time
from PIL import Image, ImageDraw
args = sys.argv[1:]
who = os.path.basename(sys.argv[0])
if who == "kasane":
    pdf, dpi, out = args[1], int(args[args.index("--dpi") + 1]), args[args.index("--output") + 1]
elif who == "pdftoppm":
    dpi, pdf, out = int(args[args.index("-r") + 1]), args[-2], args[-1] + ".png"
elif who == "mutool":
    dpi, out, pdf = int(args[args.index("-r") + 1]), args[args.index("-o") + 1], args[-2]
else:
    sys.exit(2)  # qpdf: no annotation-stripped copy
spec = json.load(open(pdf)).get(who, {{}})
mode = spec.get(str(dpi), spec.get("all", "ok"))
if mode == "fail":
    sys.exit(1)
if mode == "hang":
    time.sleep(30)
if mode == "nothing":
    sys.exit(0)
n = round(100 * dpi / 72)
w = n + (1 if mode == "size+1" else 9 if mode == "size+9" else 0)
img = Image.new("L", (w, n), 255)
d = ImageDraw.Draw(img)
s = n // 4
off = s if mode == "shift" else 0
d.rectangle([s + off, s, 2 * s + off, 2 * s], fill=0)
if mode == "tone":
    img = img.point(lambda v: max(0, v - 20))
if mode == "speckle":
    # Dark blocks of 1/24 of the side on the white margin, 1 in 64 of the
    # area: they survive the box downsampling as pixels far apart.
    b = max(1, n // 24)
    for x in range(0, w - b, 8 * b):
        d.rectangle([x, n - 2 * b, x + b - 1, n - b - 1], fill=0)
img.save(out)
'''


@unittest.skipUnless(HAVE_PIL, "Pillow not installed (KASANE_REQUIRE_PILLOW=1 makes this a failure)")
class Measures(unittest.TestCase):
    """dpx and mad on known pictures, with their boundaries."""

    def test_dpx_and_mad(self):
        from PIL import Image
        white = Image.new("L", (100, 100), 255)
        for level, want_dpx in ((255 - 48, 0.0), (255 - 49, 1.0)):
            g = Image.new("L", (100, 100), level)
            self.assertEqual(ca.dpx(white, g), want_dpx, level)
            self.assertAlmostEqual(ca.mad(white, g), 255 - level)
        # 100 of 10000 pixels 200 levels apart: dpx 1 %, mad 2.
        speck = white.copy()
        for i in range(100):
            speck.putpixel((i, 7), 55)
        self.assertAlmostEqual(ca.dpx(white, speck), 0.01)
        self.assertAlmostEqual(ca.mad(white, speck), 2.0)
        self.assertEqual(ca.dpx(white, white), 0.0)
        self.assertEqual(ca.mad(white, white), 0.0)


@unittest.skipUnless(HAVE_PIL, "Pillow not installed (KASANE_REQUIRE_PILLOW=1 makes this a failure)")
class Witnesses(unittest.TestCase):
    """compat-attribute end to end with fake renderers."""

    CASES = {
        # id: (what each renderer does, expected agreement, expected reason prefix)
        "same": ({}, "both", None),
        "kasane-only-off-at-72": ({"kasane": {"72": "shift"}}, "both", None),
        "kasane-shifted": ({"kasane": {"all": "shift"}}, "neither", None),
        "mupdf-shifted": ({"mutool": {"all": "shift"}}, "poppler", None),
        "poppler-shifted": ({"pdftoppm": {"all": "shift"}}, "mupdf", None),
        "kasane-tone": ({"kasane": {"all": "tone"}}, "neither", None),
        # Pixels far apart on 1 % of the page, the mean nearly untouched:
        # only dpx sees it.
        "kasane-speckled": ({"kasane": {"all": "speckle"}}, "neither", None),
        "one-pixel-rounding": ({"kasane": {"all": "size+1"}}, "both", None),
        "size-mismatch": ({"kasane": {"all": "size+9"}}, "not_comparable", "size_mismatch"),
        "resolution-mismatch-300": ({"mutool": {"300": "size+9"}}, "not_comparable", "size_mismatch_300"),
        "mupdf-fails": ({"mutool": {"all": "fail"}}, "not_comparable", "failed:mupdf"),
        "poppler-fails-at-300": ({"pdftoppm": {"300": "fail"}}, "not_comparable", "failed:poppler"),
        "kasane-draws-nothing": ({"kasane": {"all": "nothing"}}, "not_comparable", "no_output:kasane"),
        "poppler-hangs": ({"pdftoppm": {"all": "hang"}}, "not_comparable", "timeout:poppler"),
        "not-in-the-corpus": (None, "not_comparable", "not_fetched"),
    }

    def test_verdicts_and_failures(self):
        with tempfile.TemporaryDirectory() as d:
            bin_ = os.path.join(d, "bin")
            corpus = os.path.join(d, "corpus")
            os.makedirs(bin_)
            os.makedirs(corpus)
            for name in ("kasane", "pdftoppm", "mutool", "qpdf"):
                p = os.path.join(bin_, name)
                with open(p, "w") as fh:
                    fh.write(FAKE.format(python=sys.executable))
                os.chmod(p, os.stat(p).st_mode | stat.S_IEXEC)
            files = []
            for fid, (spec, _, _) in self.CASES.items():
                if spec is not None:
                    with open(os.path.join(corpus, fid + ".pdf"), "w") as fh:
                        json.dump(spec, fh)
                files.append(file(fid, [page(1, 0.9), page(2, 0.99)]))
            files.append(file("recovered", [page(1, 0.5)]))
            files[-1]["recovered"] = True
            files.append(file("render-error", [page(1, 0.5)], render="error"))
            report = os.path.join(d, "run.json")
            with open(report, "w") as fh:
                json.dump(run(*files), fh)
            out, summ = os.path.join(d, "pages.json"), os.path.join(d, "summary.json")
            envvars = {"KASANE_ATTR_PDFTOPPM": os.path.join(bin_, "pdftoppm"),
                       "KASANE_ATTR_MUTOOL": os.path.join(bin_, "mutool"),
                       "KASANE_ATTR_QPDF": os.path.join(bin_, "qpdf"), "KASANE_CORPUS_DIR": corpus}
            old = {k: os.environ.get(k) for k in envvars}
            os.environ.update(envvars)
            try:
                with contextlib.redirect_stdout(io.StringIO()):
                    rc = ca.main([report, os.path.join(bin_, "kasane"), "--out", out, "--summary", summ,
                                  "--timeout", "3", "--commit", "c" * 40])
            finally:
                for k, v in old.items():
                    if v is None:
                        os.environ.pop(k, None)
                    else:
                        os.environ[k] = v
            self.assertEqual(rc, 0)
            with open(out) as fh:
                rows = {r["id"]: r for r in json.load(fh)["pages"]}
            # Only the strict pages below the threshold.
            self.assertEqual(set(rows), set(self.CASES))
            self.assertTrue(all(r["page"] == 1 for r in rows.values()))
            sp = rows["kasane-speckled"]["r300"]
            self.assertGreater(sp["dpx_kp"], ca.TOLERANCE["dpx"], sp)
            self.assertLess(sp["mad_kp"], ca.TOLERANCE["mad"], sp)
            for fid, (_, agreement, reason) in self.CASES.items():
                r = rows[fid]
                self.assertEqual(r["agreement"], agreement, f"{fid}: {r}")
                if reason is None:
                    self.assertIsNone(r["reason"], fid)
                else:
                    self.assertTrue((r["reason"] or "").startswith(reason), f"{fid}: {r['reason']}")
                    self.assertIsNone(r["references_agree"], fid)
            with open(summ) as fh:
                s = json.load(fh)
            self.assertEqual(s["pages"], len(self.CASES))
            self.assertEqual(sum(s["agreement"].values()), len(self.CASES))
            # The summary holds counts only: no file id anywhere.
            text = json.dumps(s)
            for fid in self.CASES:
                self.assertNotIn(fid, text)
            self.assertEqual(s["env"]["poppler"], None)  # the fakes report no version
            # A run's attribution with unknown renderer versions cannot arm a baseline.
            self.assertIn("poppler", cr.compat_env.incomplete(s["env"], need_mupdf=True))


REAL_FIXTURES = ("render_rects", "xobject_image_gray_raw", "xobject_form_basic", "xobject_form_nested")


def real(kasane):
    """Attribution with the real renderers on committed fixtures."""
    with tempfile.TemporaryDirectory() as d:
        files = [file(f, [page(1, 0.5)]) for f in REAL_FIXTURES]
        files.append(file("render_rects", [page(9, 0.5)]))
        files[-1]["id"] = "render_rects"  # page 9 does not exist
        report = run(*files[:-1])
        report["files"][0]["metrics"]["render_pages"].append(page(9, 0.5))
        rp = os.path.join(d, "run.json")
        with open(rp, "w") as fh:
            json.dump(report, fh)
        out, summ = os.path.join(d, "pages.json"), os.path.join(d, "summary.json")
        os.environ["KASANE_CORPUS_DIR"] = os.path.join(os.path.dirname(HERE), "tests", "fixtures")
        rc = ca.main([rp, kasane, "--out", out, "--summary", summ])
        with open(out) as fh:
            rows = json.load(fh)["pages"]
        with open(summ) as fh:
            s = json.load(fh)
        bad = []
        for r in rows:
            want_nc = r["page"] == 9
            if want_nc and r["agreement"] != "not_comparable":
                bad.append(f"{r['id']} page 9 (past the end): {r['agreement']}")
            if not want_nc and r["agreement"] != "both":
                bad.append(f"{r['id']} page {r['page']}: {r['agreement']} {r.get('reason')} {r.get('r300')}")
        if len(rows) != len(REAL_FIXTURES) + 1:
            bad.append(f"{len(rows)} pages attributed")
        missing = cr.compat_env.incomplete(s["env"], need_mupdf=True)
        if missing:
            bad.append(f"environment incomplete: {missing}")
        print(f"real renderers: Poppler {s['env']['poppler']}, MuPDF {s['env']['mupdf']}: {s['agreement']}")
        for b in bad:
            print("FAIL:", b)
        return 1 if bad or rc else 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["--real"]:
        sys.exit(real(sys.argv[2]))
    unittest.main()
