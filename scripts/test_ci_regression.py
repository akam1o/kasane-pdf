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
import json
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("ci_regression", os.path.join(HERE, "ci-regression.py"))
cr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(cr)


def page(ours, ref, cssim=0.8, diff=0.01, sizes=(612, 792, 612, 792), shift_cssim=None, displaced=None):
    q = {
        "ink_ours": ours,
        "ink_ref": ref,
        "content_ssim": cssim,
        "diff_px_at_best_shift": diff,
        "sizes": list(sizes),
    }
    if shift_cssim is not None:
        q["content_ssim_at_shift"] = shift_cssim
    if displaced is not None:
        q["displaced_ink_tile"], q["displaced_ink_px"] = displaced
    return {"quality": q}


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
# Measured on the corpus (2026-10-31, runner with the placement
# metrics): the FontMatrix page before the fix, the NoRotate page
# before and after its fix, a 1-px offset scan page (legitimate).
FONTMATRIX_OLD = page(0.038, 0.012, cssim=0.07, diff=0.04, shift_cssim=0.108, displaced=(1.0, 16677))
NOROTATE_OLD = page(0.0158, 0.0161, cssim=0.264, diff=0.024, shift_cssim=0.264, displaced=(1.0, 250))
NOROTATE_FIXED = page(0.0162, 0.0161, cssim=0.975, diff=0.007, shift_cssim=0.975, displaced=(0.0, 0))
OFFSET_SCAN = page(0.05, 0.049, cssim=0.20, diff=0.005, shift_cssim=0.9, displaced=(0.0, 0))
THIN_GRID = page(0.028, 0.112, cssim=0.293, diff=0.18, shift_cssim=0.293, displaced=(0.0, 0))
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

    def test_placement_rules_catch_the_norotate_misrender(self):
        # The old gate missed it: ink ratio 0.98, best-shift diff 2.4 %.
        old = cr.quality(file("x", [NOROTATE_OLD]), self.gate)
        self.assertEqual(old, "report:content_ssim_shift,displaced_ink")
        self.assertEqual(self.q(NOROTATE_FIXED), "ok")

    def test_fontmatrix_still_enforced_and_also_reported(self):
        f = file("x", [FONTMATRIX_OLD])
        self.assertEqual(cr.quality(f, self.gate), "flag:ink_ratio")
        self.assertEqual(cr.report_rules(f, self.gate), ["content_ssim_shift", "displaced_ink", "ink_ratio_band"])

    def test_placement_rules_tolerate_offsets_and_thin_strokes(self):
        # A whole-page 1-px offset: low SSIM before the shift only.
        self.assertEqual(self.q(OFFSET_SCAN), "ok")
        # The known grid: report-only under the shifted SSIM, still the
        # known enforced flag, never a new failure.
        self.assertEqual(self.q(THIN_GRID), "flag:ink_ratio")
        base = cr.summarize(run(file("grid", [THIN_GRID])))
        self.assertEqual(check(run(file("grid", [THIN_GRID])), base)[0], 0)

    def test_placement_rules_respect_exemptions_and_old_reports(self):
        # Poppler left the page blank: exempt, whatever the placement says.
        blank = page(0.05, 0.0001, shift_cssim=0.0, displaced=(1.0, 5000))
        self.assertEqual(self.q(blank), "exempt:reference-blank")
        # /UserUnit page size: exempt.
        uu = page(0.02, 0.08, sizes=(1588, 2246, 794, 1123), shift_cssim=0.0, displaced=(1.0, 5000))
        self.assertEqual(self.q(uu), "exempt:page-size")
        # A report from before the placement metrics: not evaluated.
        self.assertEqual(self.q(page(0.0158, 0.0161, cssim=0.264, diff=0.024)), "ok")
        # A few stray pixels are not a finding.
        self.assertEqual(self.q(page(0.1, 0.1, shift_cssim=0.9, displaced=(1.0, 12))), "ok")

    def test_new_report_findings_are_listed_but_never_fail(self):
        base = cr.summarize(run(file("a", [NOROTATE_FIXED])))
        rc, out = check(run(file("a", [NOROTATE_OLD])), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("new report-only finding (not enforced, review): a: content_ssim_shift,displaced_ink", out)
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "s.md")
            check(run(file("a", [NOROTATE_OLD])), base, path)
            text = open(path).read()
        self.assertIn("| new report-only findings (review, not enforced) | 1 |", text)
        self.assertIn("| pages: `displaced_ink` (report only) | 1 |", text)

    def test_colour_rules(self):
        def coloured(tile, hue_tiles, **kw):
            p = page(0.1, 0.1, shift_cssim=0.9, displaced=(0.0, 0), **kw)
            p["quality"]["colour"] = {"worst_tile_mean": tile, "hue_tiles": hue_tiles}
            return p
        # A hue error in a flat region: both rules; a large lightness-only
        # difference: the tile rule only; small differences: nothing.
        self.assertEqual(self.q(coloured(40.0, 2)), "report:colour_hue,colour_tile")
        self.assertEqual(self.q(coloured(20.0, 0)), "report:colour_tile")
        self.assertEqual(self.q(coloured(3.0, 0)), "ok")
        # Not comparable (no colour statistics): not evaluated.
        self.assertEqual(self.q(page(0.1, 0.1, shift_cssim=0.9, displaced=(0.0, 0))), "ok")
        # Poppler blank: exempt before any colour rule.
        blank = coloured(80.0, 5)
        blank["quality"]["ink_ref"] = 0.0
        self.assertEqual(self.q(blank), "exempt:reference-blank")
        # A font-substituted page: colour rules only (its glyph shapes
        # are a reported approximation, its colours are not).
        f = file("x", [coloured(40.0, 0)], approximate=True)
        f["metrics"]["render_font_substitutions"] = ["Helvetica -> Liberation Sans"]
        f["metrics"]["render_pages"][0]["quality"]["content_ssim_at_shift"] = 0.1
        self.assertEqual(cr.report_rules(f, self.gate), ["colour_tile"])
        self.assertEqual(cr.quality(f, self.gate), "n/a")
        # A colour approximation (ICC / CMYK fallback): never judged.
        f["metrics"]["render_color_approximations"] = ["ICCBased Lab PCS -> /Alternate"]
        self.assertEqual(cr.report_rules(f, self.gate), [])

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

    def with_semantic(self, f, classes, f1_annots=None):
        f = dict(f)
        f["roundtrip"] = {
            "full": True,
            "semantic": {"class": "pass", "checks": [{"category": c, "class": k} for c, k in classes.items()]},
        }
        if f1_annots is not None:
            f["metrics"] = dict(f["metrics"], text_char_f1_with_annots=f1_annots)
        return f

    def test_semantic_categories_are_report_only(self):
        # v0.4.0 baseline: seven categories, no annotation F1. A run with
        # the document-level categories and the new metric reads, and a
        # pass -> fail in any category is listed, never a regression.
        old = self.with_semantic(file("a", [NORMAL]), {"acroform": "pass", "outlines": "pass"})
        base = cr.summarize(run(old))
        new = self.with_semantic(
            file("a", [NORMAL]),
            {"acroform": "fail", "outlines": "pass", "xfa": "fail", "metadata": "pass"},
            f1_annots=0.5,
        )
        rc, out = check(run(new), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("semantic acroform pass -> fail", out)
        # The new categories are summarised for the next baseline.
        self.assertEqual(cr.summarize(run(new))["files"]["a"]["semantic"]["xfa"], "fail")
        # And a baseline with them, against a run without (older runner).
        rc, out = check(run(old), cr.summarize(run(new)))
        self.assertEqual(rc, 0, out)

    def test_report_only_semantic_categories_never_fail(self):
        # PDF 2.0 categories (report_only): listed when they are not a
        # pass against a baseline that predates them, or lose a pass
        # against one that has them; never a regression, and absent from
        # the enforced "semantic" map.
        old = self.with_semantic(file("a", [NORMAL]), {"acroform": "pass"})
        base = cr.summarize(run(old))
        new = self.with_semantic(file("a", [NORMAL]), {"acroform": "pass"})
        new["roundtrip"]["semantic"]["report_only"] = [
            {"category": "version", "class": "pass"},
            {"category": "associated_files", "class": "fail"},
            {"category": "dparts", "class": "not_applicable"},
        ]
        rc, out = check(run(new), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("report-only semantic associated_files: fail (no baseline)", out)
        self.assertNotIn("dparts", out)
        summary = cr.summarize(run(new))["files"]["a"]
        self.assertNotIn("associated_files", summary["semantic"])
        self.assertEqual(summary["semantic_report"]["associated_files"], "fail")
        good = self.with_semantic(file("a", [NORMAL]), {"acroform": "pass"})
        good["roundtrip"]["semantic"]["report_only"] = [{"category": "associated_files", "class": "pass"}]
        rc, out = check(run(new), cr.summarize(run(good)))
        self.assertEqual(rc, 0, out)
        self.assertIn("report-only semantic associated_files pass -> fail", out)


    def test_transformed_is_listed_never_a_pass(self):
        # T1 moves a report-only `version` fail to transformed; listed for
        # review with its rule, never a regression, and not a pass.
        old = self.with_semantic(file("a", [NORMAL]), {"acroform": "pass"})
        old["roundtrip"]["semantic"]["report_only"] = [{"category": "version", "class": "fail"}]
        new = self.with_semantic(file("a", [NORMAL]), {"acroform": "pass"})
        new["roundtrip"]["semantic"]["report_only"] = [
            {"category": "version", "class": "transformed", "rules": ["T1 version raised"]},
            {"category": "signatures", "class": "not_applicable"},
        ]
        rc, out = check(run(new), cr.summarize(run(old)))
        self.assertEqual(rc, 0, out)
        self.assertIn("semantic version fail -> transformed (T1 version raised)", out)
        self.assertNotIn("signatures", out)
        summary = cr.summarize(run(new))
        self.assertEqual(summary["files"]["a"]["semantic_rules"], {"version": ["T1 version raised"]})
        # Against a baseline that has it under the same rule: nothing.
        rc, out = check(run(new), summary)
        self.assertNotIn("transformed", out)
        # The same class under another rule set is listed again.
        other = json.loads(json.dumps(new))
        other["roundtrip"]["semantic"]["report_only"][0]["rules"] = ["T2 usage rights removed"]
        rc, out = check(run(other), summary)
        self.assertIn("version transformed -> transformed (T2 usage rights removed)", out)
        # An enforced category: pass -> transformed is listed once, with
        # the rule, and is not a regression.
        enf = self.with_semantic(file("a", [NORMAL]), {"acroform": "transformed"})
        enf["roundtrip"]["semantic"]["checks"][0]["rules"] = ["T2 usage rights removed"]
        rc, out = check(run(enf), cr.summarize(run(old)))
        self.assertEqual(rc, 0, out)
        self.assertEqual(out.count("acroform"), 1, out)
        self.assertIn("semantic acroform pass -> transformed (T2 usage rights removed)", out)
        # No baseline record of the file's semantic at all.
        rc, out = check(run(enf), cr.summarize(run(file("a", [NORMAL]))))
        self.assertIn("acroform unrecorded -> transformed", out)

    def test_semantic_moves_into_failure_are_listed_from_any_class(self):
        # Against a baseline that records a category, a move into fail,
        # error or unsupported is listed whatever the class was (n/a ->
        # fail: the rewrite added the structure), and a transformation
        # lost is listed; nothing fails the gate.
        def f(enforced, report, rules=None):
            x = self.with_semantic(file("a", [NORMAL]), enforced)
            x["roundtrip"]["semantic"]["report_only"] = [
                dict({"category": c, "class": k}, **({"rules": rules} if k == "transformed" else {}))
                for c, k in report.items()
            ]
            return x
        base = cr.summarize(run(f({"acroform": "not_applicable"},
                                  {"signatures": "not_applicable", "version": "transformed"},
                                  ["T1 version raised"])))
        rc, out = check(run(f({"acroform": "fail"}, {"signatures": "fail", "version": "fail"})), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("semantic acroform not_applicable -> fail", out)
        self.assertIn("report-only semantic signatures not_applicable -> fail", out)
        self.assertIn("report-only semantic version transformed -> fail", out)
        # Unchanged, or n/a -> pass: nothing listed.
        rc, out = check(run(f({"acroform": "pass"}, {"signatures": "not_applicable", "version": "transformed"},
                              ["T1 version raised"])), base)
        self.assertNotIn("semantic", out)

    def test_figures_by_config_are_separate_namespaces(self):
        def rt(full=True, sem="pass", structure="Pass", **extra):
            return dict(
                {"full": full, "reparse": {"class": "Pass"}, "structure": {"class": structure},
                 "semantic": {"class": sem, "checks": []}},
                **extra,
            )
        a = dict(file("a", [NORMAL]), roundtrip=rt())
        # Refused by default for its signature; rewritten and transformed
        # under D1, the graph difference exactly the declared one.
        b = dict(file("b", [NORMAL]), rewrite={"class": "unsupported"}, rewrite_refusal="signature",
                 roundtrip={"rewrite": {"class": "Unsupported"}})
        b["rewrite_options"] = {
            "remove-usage-rights": {
                "rewrite": "pass", "declared": ["T2 usage rights removed"],
                "roundtrip": rt(full=False, sem="transformed", structure="Fail",
                                structure_declared=True, full_declared=True),
            },
        }
        a["rewrite_options"] = {"remove-usage-rights": {"rewrite": "pass", "roundtrip": rt()}}
        # A script refusal, and a file that never reached the rewrite.
        c = dict(file("c", [NORMAL]), rewrite={"class": "unsupported"}, rewrite_refusal="script",
                 roundtrip={"rewrite": {"class": "Unsupported"}})
        c["rewrite_options"] = {"remove-usage-rights": {"rewrite": "unsupported", "refusal": "script",
                                                        "roundtrip": {"rewrite": {"class": "Unsupported"}}}}
        d = dict(file("d", [NORMAL]), parse={"class": "error"}, rewrite={"class": "skip"})
        a["policy_check"] = {"identical": True, "version_agrees": True,
                             "report": {"script": 1, "navigation": 2},
                             "inventory": {"agree": True}}
        b["rewrite_options"]["remove-usage-rights"]["report"] = {"script": 2, "unclassified": 1}
        b["rewrite_options"]["remove-usage-rights"]["inventory"] = {"agree": False, "runner_error": "limit"}
        b["rewrite_options"]["remove-usage-rights"]["usage_rights"] = {"outcome": "removed", "agree": True}
        c["rewrite_options"]["remove-usage-rights"]["usage_rights"] = {"outcome": "refused", "agree": None}
        figs = cr.summarize(run(a, b, c, d))["aggregate"]["by_config"]
        dflt, d1 = figs["default"], figs["remove-usage-rights"]
        self.assertEqual((d1["usage_rights_removed"], d1["usage_rights_refused"], d1["usage_rights_agree"],
                          d1["usage_rights_disagree"]), (1, 1, 1, 0))
        # T2: the declared structure is not enough when the signatures are
        # not transformed (a declared path with an undeclared value).
        bad = json.loads(json.dumps(b))
        bad["rewrite_options"]["remove-usage-rights"]["roundtrip"]["signatures_transformed"] = False
        d1b = cr.summarize(run(a, bad, c, d))["aggregate"]["by_config"]["remove-usage-rights"]
        self.assertEqual((d1b["full_pass"], d1b["full_pass_verified"], d1b["signatures_transformed"]), (2, 1, 0))
        good = json.loads(json.dumps(b))
        good["rewrite_options"]["remove-usage-rights"]["roundtrip"]["signatures_transformed"] = True
        d1g = cr.summarize(run(a, good, c, d))["aggregate"]["by_config"]["remove-usage-rights"]
        self.assertEqual((d1g["full_pass_verified"], d1g["signatures_transformed"]), (2, 1))
        # A usage-rights report that disagrees with the runner is not verified.
        dis = json.loads(json.dumps(good))
        dis["rewrite_options"]["remove-usage-rights"]["usage_rights"] = {"outcome": "removed", "agree": False}
        d1d = cr.summarize(run(a, dis, c, d))["aggregate"]["by_config"]["remove-usage-rights"]
        self.assertEqual((d1d["full_pass_verified"], d1d["usage_rights_disagree"]), (1, 1))
        b_pc = json.loads(json.dumps(dis))
        b_pc["policy_check"] = {"identical": True}
        rc, out = check(run(a, b_pc, c, d), cr.summarize(run(a, b, c, d)))
        self.assertIn("b: remove-usage-rights: the usage-rights report disagrees", out)
        self.assertEqual((dflt["files"], dflt["full_pass"], dflt["signature_refusal"], dflt["script_refusal"]), (3, 1, 1, 1))
        self.assertEqual((dflt["semantic_pass"], dflt["semantic_transformed"]), (1, 0))
        self.assertEqual((d1["files"], d1["rewrite_pass"], d1["full_pass"], d1["full_pass_verified"]), (3, 2, 2, 2))
        self.assertEqual((d1["semantic_pass"], d1["semantic_transformed"], d1["script_refusal"]), (1, 1, 1))
        self.assertEqual((d1["graph_rt"], d1["graph_rt_declared"], d1["full_rt"], d1["full_rt_declared"]), (1, 1, 1, 2))
        self.assertEqual((dflt["default_identical"], dflt["default_differs"], dflt["version_agrees"]), (1, 0, 1))
        self.assertEqual((dflt["report_files"], dflt["report_script"], dflt["report_navigation"]), (1, 1, 2))
        self.assertEqual((dflt["inventory_checked"], dflt["inventory_agree"]), (1, 1))
        self.assertEqual((d1["report_script"], d1["report_unclassified"], d1["inventory_agree"],
                          d1["inventory_runner_error"]), (2, 1, 0, 1))
        # The enforced aggregate and the gate read the default only: an
        # option run that loses everything changes nothing in `check`.
        base = cr.summarize(run(a, b, c, d))
        worse = json.loads(json.dumps(b))
        worse["rewrite_options"]["remove-usage-rights"] = {"rewrite": "error", "roundtrip": None}
        rc, out = check(run(a, worse, c, d), base)
        self.assertEqual(rc, 0, out)
        # Listed for review in its own namespace, never a regression.
        self.assertIn("rewrite configuration change (not enforced, review): b: remove-usage-rights", out)
        self.assertFalse([l for l in out.splitlines() if l.startswith("REGRESSION") and "remove-usage-rights" in l])
        self.assertEqual(base["aggregate"]["rewrite_pass"], 1)

    def test_the_defaults_identity_and_report_are_enforced(self):
        good = dict(file("a", [NORMAL]), policy_check={
            "identical": True, "version_agrees": True, "inventory": {"agree": True}})
        base = cr.summarize(run(good))
        rc, out = check(run(good), base)
        self.assertEqual(rc, 0, out)
        for change, msg in [
            ({"identical": False}, "differs from rewrite"),
            ({"inventory": {"agree": False}}, "disagrees with the runner's inventory"),
            ({"version_agrees": False}, "version change disagrees"),
        ]:
            bad = json.loads(json.dumps(good))
            bad["policy_check"].update(change)
            rc, out = check(run(bad), base)
            self.assertEqual(rc, 1, out)
            self.assertIn(msg, out)
        # The runner's own limit is not a disagreement; an older runner
        # (no policy_check) reads as before.
        lim = json.loads(json.dumps(good))
        lim["policy_check"]["inventory"] = {"agree": False, "runner_error": "limit"}
        self.assertEqual(check(run(lim), base)[0], 0)
        self.assertEqual(check(run(file("a", [NORMAL])), base)[0], 0)
        # Another configuration's disagreement is listed, not enforced.
        opt = json.loads(json.dumps(good))
        opt["rewrite_options"] = {"carry": {"rewrite": "pass", "inventory": {"agree": False}}}
        rc, out = check(run(opt), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("carry: the report disagrees", out)

    def test_a_missing_category_and_a_lost_option_gain_are_listed(self):
        # A semantic category the baseline records and the run lacks is
        # listed; so is a configuration's lost Full PASS, rewrite or
        # semantic pass — in its own namespace, never failing the gate.
        old = self.with_semantic(file("a", [NORMAL]), {"acroform": "pass", "outlines": "pass"})
        old["rewrite_options"] = {"carry": {"rewrite": "pass", "roundtrip": {
            "full": True, "reparse": {"class": "Pass"}, "structure": {"class": "Pass"},
            "semantic": {"class": "pass", "checks": []}}}}
        base = cr.summarize(run(old))
        self.assertEqual(base["files"]["a"]["options"]["carry"],
                         {"rewrite": "pass", "full_pass": True, "semantic": "pass"})
        new = self.with_semantic(file("a", [NORMAL]), {"acroform": "pass"})
        new["rewrite_options"] = {"carry": {"rewrite": "unsupported", "refusal": "unknown_action"}}
        rc, out = check(run(new), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("semantic outlines pass -> missing", out)
        self.assertIn("carry: full_pass True -> False", out)
        self.assertIn("carry: rewrite pass -> unsupported", out)
        self.assertIn("carry: semantic pass -> None", out)
        # The same run against itself: nothing listed.
        rc, out = check(run(new), cr.summarize(run(new)))
        self.assertNotIn("carry:", out)
        self.assertNotIn("-> missing", out)

    def test_a_configuration_or_policy_check_not_run_is_listed(self):
        f = dict(file("a", [NORMAL]), policy_check={"identical": True, "version_agrees": True,
                                                    "inventory": {"agree": True}})
        f["rewrite_options"] = {"carry": {"rewrite": "pass", "roundtrip": {"reparse": {"class": "Pass"}}}}
        base = cr.summarize(run(f))
        rc, out = check(run(file("a", [NORMAL])), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("rewrite configuration not run: carry", out)
        self.assertIn("policy check not run", out)
        rc, out = check(run(f), base)
        self.assertNotIn("not run", out)

    def test_text_agreement_is_recorded_and_report_only(self):
        def with_text(f1, f1a):
            f = file("a", [NORMAL])
            f["metrics"] = dict(f["metrics"], text_char_f1=f1, text_char_f1_with_annots=f1a)
            return f

        base = cr.summarize(run(with_text(0.98765, 0.99)))
        self.assertEqual(base["files"]["a"]["text_f1"], 0.9877)
        self.assertEqual(base["files"]["a"]["text_f1_annots"], 0.99)
        # A large drop is listed, never a regression; a small one is quiet.
        rc, out = check(run(with_text(0.5, 0.99)), base)
        self.assertEqual(rc, 0, out)
        self.assertIn("text agreement drop (not enforced, review): a: text_f1 0.9877 -> 0.5", out)
        rc, out = check(run(with_text(0.97, 0.98)), base)
        self.assertNotIn("text agreement drop", out)
        # A baseline without the fields (0.4.0) compares nothing.
        old = cr.summarize(run(file("a", [NORMAL])))
        self.assertIsNone(old["files"]["a"]["text_f1"])
        rc, out = check(run(with_text(0.1, 0.1)), old)
        self.assertEqual(rc, 0, out)
        self.assertNotIn("text agreement drop", out)

if __name__ == "__main__":
    unittest.main()
