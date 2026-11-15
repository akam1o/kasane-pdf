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

ENV = {"commit": "c" * 40, "os": "linux-ubuntu-26.04", "arch": "x86_64", "features": "cjk-fallback",
       "poppler": "26.01.0", "mupdf": "1.27.0", "qpdf": "12.3.2"}
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


def att(*rows, env=ENV, threshold=0.95):
    pages = []
    for fid, pg, ag, d, m in rows:
        r = r300(ag, d, m)
        got, refs = ca.verdict(r)
        assert got == ag, (ag, d, m, got)
        pages.append({"id": fid, "page": pg, "ssim": 0.9, "class": "kasane", "agreement": ag,
                      "references_agree": refs, "reason": "failed:mupdf" if ag == "not_comparable" else None,
                      "r300": r})
    return {"schema": 2, "env": env, "threshold": threshold, "tolerance": TOL, "max_pages_per_file": 8,
            "pages": pages}


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
