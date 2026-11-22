#!/usr/bin/env python3
"""Regression gate over a kasane-compat run report.

Two modes:

  ci-regression.py summarize RUN.json > baseline.json
      Reduce a run report to the aggregate counts and the per-file
      verdicts / stage classes / round-trip classes — no paths, no
      images, no timings — so the result can be committed as a baseline.

  ci-regression.py check RUN.json baseline.json
      Exit 1 when any file present in both got *worse*: a stage that was
      `pass` and no longer is, a round-trip check that was PASS and is now
      FAIL, a `reject` file that now crashes or times out, or any crash /
      timeout at all. Improvements and files only in one side are
      reported but never fail the gate.

  ci-regression.py quality RUN.json
      Print the exact-render quality-gate evaluation (every flag, the
      exemptions applied, the distribution) without comparing.

Exact-render quality gate (compat/quality-gate.json,
docs/plans/quality-gate.md): every page of a file whose strict render
passed without a reported approximation is compared with Poppler by
the rules in the config. A file is `ok`, `flag:<rule>` (an enforced
rule fired), `report:<rule>` (a report-only rule fired), `exempt:<why>`
(the comparison is not meaningful — Poppler drew nothing, the page
sizes differ) or `n/a` (not an exact render, or no reference). `check`
fails on a new silent misrender: a file flagged by an enforced rule
that the baseline records as not flagged (or does not contain), or a
known flagged file with more flagged pages than the baseline records.
Flags already in the baseline are listed as known, not failed — but
their number may not exceed `reviewed_known_flags` in the gate config,
so regenerating a baseline cannot quietly absorb a new flag: raising
the number is a reviewed change with its reason in
docs/plans/quality-gate.md.

  ci-regression.py check RUN.json baseline.json --summary-md FILE
      Also append a markdown summary of the quality gate to FILE (the
      nightly passes $GITHUB_STEP_SUMMARY). Counts only, no file names.

  ci-regression.py check RUN.json baseline.json --env ENV.json
          [--attribution PAGES.json] [--require-same-env]
      Also compare the numeric measures (M1-c, docs/compat-evaluation.md)
      when the baseline records them: per-page SSIM and content SSIM,
      page-1 text F1, pages compared, and the second-witness agreement
      of every page below the attribution threshold
      (scripts/compat-attribute.py), page by page through a ledger keyed
      by (case, page) — never by rank (M2). They are compared only between runs
      of the same environment (scripts/compat_env.py: platform, Poppler,
      MuPDF, features). In another environment they are reported as not
      compared; with --require-same-env (the nightly) that is a
      regression, and so is a run without --env or --attribution when
      the baseline has them.

  ci-regression.py env [--commit SHA] [--features F] > ENV.json
      The measurement environment of this machine.

  ci-regression.py update-baseline RUN.json baseline.json --env ENV.json
          [--attribution PAGES.json] [--write] [--allow-removed]
          [--accept-regressions] [--ids]
      The only way to change a baseline: prints what the new baseline
      would change (counts; --ids lists the files, for a local review
      only), and with --write writes it — refused when files would be
      removed (without --allow-removed), when `check` finds regressions
      against the old one (without --accept-regressions; the first
      measurements of pages an older baseline did not attribute alone:
      --accept-unverified, which accepts nothing else), when the
      environment is incomplete, or when the attribution does not cover
      every page below its threshold (capped per file, truncated, or
      missing one). No workflow writes a baseline
      (scripts/check-workflows.py).

Every run is validated first: a report or baseline that is not the
expected shape, has duplicate ids or keys, records a page twice, holds
a non-finite number (NaN, Infinity) or a measure out of its range
(SSIM outside -1..1, text F1 outside 0..1), or a label its measures do
not give, is rejected (exit 2), never compared. A file of the baseline
missing from the run is a regression: a removed case is not an
improvement.

Only aggregates and cause ids are ever written; the corpus itself and
rendered images stay out of git.
"""
import argparse
import json
import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import compat_env  # noqa: E402
import compat_verdict  # noqa: E402

STAGES = ("parse", "render", "text", "rewrite")
RT_CHECKS = ("reparse", "structure", "render", "text", "annotations", "external_valid", "external_no_new")
RANK = {"pass": 0, "skip": 0, "mismatch": 1, "unsupported": 2, "error": 3, "timeout": 4, "crash": 5}


GATE_CONFIG = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compat", "quality-gate.json")


def load_gate():
    with open(GATE_CONFIG) as fh:
        return json.load(fh)


# Reviewed differences (compat/reviewed-differences.json): a page whose
# one measure moved past its threshold, investigated and accepted in a
# review, within exactly the reviewed range. Each entry names the case,
# its input (SHA-256), the page, the measure, the environment it was
# reviewed in, the baseline value and the reviewed value; the original
# regression is still printed, marked accepted, and anything else —
# another input, environment or baseline value, a value past the
# reviewed one, another measure or page — stays a regression.
REVIEWED_CONFIG = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compat",
                               "reviewed-differences.json")
REVIEWED_METRICS = {"mad_kp": "max", "mad_km": "max", "dpx_kp": "max", "dpx_km": "max", "cssim": "min"}
REVIEWED_FIELDS = ("id", "sha256", "page", "metric", "measure", "env", "baseline", "reviewed", "reason", "review")


def load_reviewed():
    with open(REVIEWED_CONFIG) as fh:
        return json.load(fh).get("differences") or []


class Reviewed:
    """The reviewed differences that apply to one check: `accepts` is
    asked for each regression of a measure an entry may cover, records
    what it accepted (`accepted`) and why an entry did not apply
    (`refused`)."""

    def __init__(self, entries, env, shas):
        self.entries, self.env, self.shas = list(entries or []), env, shas
        # `accepted`: the printed lines; `original`: the regressions they
        # are under the original rule (a baseline update still needs
        # --accept-regressions for them).
        self.accepted, self.original, self.refused = [], [], []

    @staticmethod
    def env_gaps(env):
        """The environment fields `env` lacks or leaves blank (the
        compared ones, and every field of the reference fingerprint)."""
        if not isinstance(env, dict):
            return ["environment"]
        gaps = [k for k in compat_env.COMPARED if k != "reference" and not Reviewed.filled(env.get(k))]
        ref = env.get("reference")
        if not isinstance(ref, dict):
            gaps.append("reference")
        else:
            gaps += [f"reference.{k}" for k in compat_env.REFERENCE_FIELDS if not Reviewed.filled(ref.get(k))]
        return gaps

    @staticmethod
    def filled(v):
        """A non-blank string or a finite number (not a boolean)."""
        if isinstance(v, str):
            return bool(v.strip())
        return isinstance(v, (int, float)) and not isinstance(v, bool) and math.isfinite(v)

    def why_not(self, e, was, now):
        if not isinstance(e, dict):
            return "evidence missing: not an entry"
        strings = ("id", "sha256", "metric", "measure", "reason", "review")
        missing = [k for k in REVIEWED_FIELDS if k not in e
                   or (k in strings and not (isinstance(e[k], str) and e[k].strip()))
                   or (k in ("page", "baseline", "reviewed") and not self.filled(e[k]))
                   or (k == "env" and not (isinstance(e[k], dict) and e[k]))]
        if missing:
            return "evidence missing: " + ", ".join(missing)
        if not isinstance(e["sha256"], str) or len(e["sha256"]) != 64 \
                or any(c not in "0123456789abcdef" for c in e["sha256"]):
            return "evidence missing: sha256 is not a SHA-256"
        if not isinstance(e["page"], int) or isinstance(e["page"], bool):
            return "evidence missing: page is not a number"
        gaps = self.env_gaps(e["env"])
        if gaps:
            return "evidence missing: env " + ", ".join(gaps)
        bound = REVIEWED_METRICS.get(e["metric"])
        nums = (was, now, e["baseline"], e["reviewed"])
        if bound is None or not all(isinstance(v, (int, float)) and math.isfinite(v) for v in nums):
            return "not a reviewed measure or value"
        sha = self.shas.get(e["id"])
        if sha is None:
            return "the input's SHA-256 is not in the run"
        if sha != e["sha256"]:
            return "another input (SHA-256)"
        if self.env is None or self.env_gaps(self.env):
            return "the run's environment is incomplete: " + ", ".join(self.env_gaps(self.env) or ["none given"])
        diff = compat_env.differences(e["env"], self.env)
        if diff:
            return "another environment: " + ", ".join(diff)
        if was != e["baseline"]:
            return f"another baseline value ({was}, reviewed from {e['baseline']})"
        if (now > e["reviewed"]) if bound == "max" else (now < e["reviewed"]):
            return f"past the reviewed value {e['reviewed']}"
        return None

    def accepts(self, fid, pg, metric, was, now, line, record=None):
        for e in self.entries:
            if e.get("id") == fid and e.get("page") == pg and e.get("metric") == metric:
                why = self.why_not(e, was, now)
                if why:
                    self.refused.append(f"{line}: reviewed difference not applied ({why})")
                    return False
                msg = (f"{line} — accepted within the reviewed difference ({metric} {e['baseline']} -> "
                       f"{'at most' if REVIEWED_METRICS[metric] == 'max' else 'at least'} {e['reviewed']}; "
                       f"{e['review']})")
                self.accepted.append(msg)
                self.original.append(line)
                if record is not None:
                    record.append(msg)
                return True
        return False


def page_findings(q, gate):
    """Rules fired on one page's quality record, or an exemption."""
    ev = gate["evaluate"]
    io, ir = q.get("ink_ours", 0.0), q.get("ink_ref", 0.0)
    sizes = q.get("sizes") or []
    if len(sizes) == 4 and min(sizes[2], sizes[3]) > 0:
        rel = max(abs(sizes[0] - sizes[2]) / sizes[2], abs(sizes[1] - sizes[3]) / sizes[3])
        if rel > ev["max_size_rel_diff"]:
            return [], "page-size"
    if ir < ev["blank_reference_ink"] and io >= ev["min_ink"]:
        return [], "reference-blank"
    fired = []
    for name, r in gate["rules"].items():
        if r["kind"] == "ink_ratio":
            hit = io >= ev["min_ink"] and ir >= ev["min_ink"] and not (r["low"] <= io / ir <= r["high"])
        elif r["kind"] == "content_ssim":
            hit = q.get("content_ssim", 1.0) < r["max"] and q.get("diff_px_at_best_shift", 0.0) > r["min_diff_at_best_shift"]
        elif r["kind"] == "content_ssim_shift":
            # Measured only by newer runners (None: not evaluated).
            v = q.get("content_ssim_at_shift")
            hit = v is not None and io >= ev["min_ink"] and ir >= ev["min_ink"] and v < r["max"]
        elif r["kind"] == "displaced_ink":
            t, n = q.get("displaced_ink_tile"), q.get("displaced_ink_px")
            hit = t is not None and n is not None and t > r["min_tile_share"] and n >= r["min_px"]
        elif r["kind"] == "colour_tile":
            # ΔE00 statistics (runner colour.rs); None: not comparable.
            c = q.get("colour")
            hit = c is not None and (c.get("worst_tile_mean") or 0.0) > r["max_tile_mean"]
        elif r["kind"] == "colour_hue":
            c = q.get("colour")
            hit = c is not None and (c.get("hue_tiles") or 0) >= r["min_hue_tiles"]
        else:
            raise ValueError(f"unknown rule kind {r['kind']!r}")
        if hit:
            fired.append(name)
    return fired, None


def is_exact(f):
    """Strict render passed with no reported approximation."""
    return (
        (f.get("render") or {}).get("class") == "pass"
        and not f.get("recovered")
        and not f.get("render_approximate")
    )


def quality_detail(f, gate):
    """`(class, pages compared, pages flagged by an enforced rule,
    pages with a report-only finding, pages exempt)` for one file."""
    if not is_exact(f):
        return "n/a", 0, 0, 0, 0
    m = f.get("metrics") or {}
    pages = [p.get("quality") for p in (m.get("render_pages") or []) if p.get("quality")]
    if not pages and m.get("render_quality"):
        pages = [m["render_quality"]]
    if not pages:
        return "n/a", 0, 0, 0, 0
    enforced, reported, exempt = set(), set(), set()
    n_flag = n_report = n_exempt = 0
    for q in pages:
        fired, ex = page_findings(q, gate)
        if ex:
            exempt.add(ex)
            n_exempt += 1
        hard = [r for r in fired if gate["rules"][r]["enforced"]]
        soft = [r for r in fired if not gate["rules"][r]["enforced"]]
        enforced.update(hard)
        reported.update(soft)
        n_flag += bool(hard)
        n_report += bool(soft) and not hard
    if enforced:
        cls = "flag:" + ",".join(sorted(enforced))
    elif reported:
        cls = "report:" + ",".join(sorted(reported))
    elif exempt:
        cls = "exempt:" + ",".join(sorted(exempt))
    else:
        cls = "ok"
    return cls, len(pages), n_flag, n_report, n_exempt


COLOUR_KINDS = ("colour_tile", "colour_hue")


def colour_comparable(f):
    """A strict render whose only reported approximations (if any) are
    font substitutions — they change glyph shapes, never colours — so
    the colour rules apply even though the page is not exact. Colour
    and image approximations (ICC / CMYK fallbacks, reduced JPEG
    decoding) keep it out."""
    m = f.get("metrics") or {}
    return (
        (f.get("render") or {}).get("class") == "pass"
        and not f.get("recovered")
        and not m.get("render_color_approximations")
        and not m.get("render_image_approximations")
    )


def report_rules(f, gate):
    """Report-only rules fired on any compared page: every rule on an
    exact render; the colour rules alone on a colour-comparable one.
    (An enforced colour rule would be evaluated by `quality`, on exact
    renders only — widen that too before enforcing one.)"""
    exact = is_exact(f)
    if not exact and not colour_comparable(f):
        return []
    m = f.get("metrics") or {}
    fired = set()
    for p in m.get("render_pages") or []:
        q = p.get("quality")
        if q:
            rules, _ = page_findings(q, gate)
            fired.update(
                r
                for r in rules
                if not gate["rules"][r]["enforced"]
                and (exact or gate["rules"][r]["kind"] in COLOUR_KINDS)
            )
    return sorted(fired)


def quality(f, gate):
    """The file's gate class (see the module docstring)."""
    return quality_detail(f, gate)[0]


# Numeric regression thresholds (M1-c). Runs of one environment are
# deterministic: three Linux nightlies (c593bb8, 09a8e78, e7d52aa; 1147
# pages) agree exactly on every SSIM, content SSIM and text F1, so any
# move is an engine change; the thresholds sit above the 4-decimal
# rounding of the baseline and below the smallest change worth a review.
SSIM_DROP = 0.002
CONTENT_SSIM_DROP = 0.01
TEXT_F1_DROP_ENFORCED = 0.02
# An open page (outside tolerance of both witnesses at 300 dpi) whose
# difference from the nearer witness grows by more than this is a
# regression although its class does not change.
OPEN_DPX_GROWTH = 0.002
OPEN_MAD_GROWTH = 0.5
WITHIN = ("both", "poppler", "mupdf")


class Invalid(Exception):
    """A report or baseline that cannot be compared."""


def _finite_tree(o, where):
    if isinstance(o, float) and not math.isfinite(o):
        raise Invalid(f"non-finite number at {where}")
    if isinstance(o, dict):
        for k, v in o.items():
            _finite_tree(v, f"{where}.{k}")
    elif isinstance(o, list):
        for i, v in enumerate(o):
            _finite_tree(v, f"{where}[{i}]")


def _in_range(v, lo, hi):
    return v is None or (isinstance(v, (int, float)) and not isinstance(v, bool) and lo <= v <= hi)


def validate_run(run):
    if not isinstance(run, dict) or not isinstance(run.get("files"), list):
        raise Invalid("run report: no 'files' list")
    seen = set()
    for i, f in enumerate(run["files"]):
        if not isinstance(f, dict) or not isinstance(f.get("id"), str) or not f["id"]:
            raise Invalid(f"run report: file {i} has no id")
        if f["id"] in seen:
            raise Invalid("run report: duplicate file id")
        seen.add(f["id"])
        for s in STAGES:
            if s in f and not isinstance(f[s], dict):
                raise Invalid(f"run report: stage {s} of file {i} is not a record")
        records = (f.get("metrics") or {}).get("render_pages") or []
        if not isinstance(records, list) or any(not isinstance(p, dict) for p in records):
            raise Invalid(f"run report: file {i} has a page record that is not a record")
        pages = [p.get("page") for p in records]
        if any(not isinstance(pg, int) or isinstance(pg, bool) or pg < 1 for pg in pages):
            raise Invalid(f"run report: file {i} has a page that is not a number from 1")
        if len(pages) != len(set(pages)):
            raise Invalid(f"run report: file {i} records a page twice")
        _finite_tree(f, f"files[{i}]")
        for p in records:
            # A compared page has its SSIM: a record without one would
            # drop out of every comparison unseen.
            if p.get("ssim") is None or not (_in_range(p.get("ssim"), -1, 1) and _in_range(
                    (p.get("quality") or {}).get("content_ssim"), -1, 1)):
                raise Invalid(f"run report: file {i} has a page without an SSIM, or one outside -1..1")
        for key in ("text_char_f1", "text_char_f1_with_annots"):
            if not _in_range((f.get("metrics") or {}).get(key), 0, 1):
                raise Invalid(f"run report: file {i} has a text F1 outside 0..1")


def validate_base(base):
    if not isinstance(base, dict) or not isinstance(base.get("files"), dict):
        raise Invalid("baseline: no 'files' map")
    for fid, o in base["files"].items():
        if not isinstance(o, dict) or not isinstance(o.get("stages"), dict):
            raise Invalid("baseline: a file without stages")
        for pg, v in (o.get("pages") or {}).items():
            if not pg.isdigit() or int(pg) < 1 or not isinstance(v, dict):
                raise Invalid("baseline: a page that is not a number from 1")
            if not (_in_range(v.get("ssim"), -1, 1) and _in_range(v.get("cssim"), -1, 1)):
                raise Invalid("baseline: an SSIM outside -1..1")
        for key in ("text_f1", "text_f1_annots"):
            if not _in_range(o.get(key), 0, 1):
                raise Invalid("baseline: a text F1 outside 0..1")
        for pg, a in (o.get("attribution") or {}).items():
            if not pg.isdigit() or int(pg) < 1 or not isinstance(a, dict):
                raise Invalid("baseline: an attributed page that is not a number from 1")
            if a.get("agreement") not in compat_verdict.AGREEMENTS:
                raise Invalid("baseline: an unknown agreement")
            if a["agreement"] != "not_comparable":
                ms = [a.get(k) for k in ("dpx_kp", "mad_kp", "dpx_km", "mad_km")]
                if not compat_verdict.finite(*ms):
                    raise Invalid("baseline: an agreement without its measures")
                if a["agreement"] != _agreement(a):
                    raise Invalid("baseline: an agreement its measures do not give")
    _finite_tree(base, "baseline")
    meta = base.get("meta") or {}
    has_pages = any("pages" in o for o in base["files"].values())
    if has_pages and not meta.get("env"):
        raise Invalid("baseline: page measures without a recorded environment")


def _agreement(m):
    """The agreement per witness measures (kp: Poppler, km: MuPDF)."""
    wp = compat_verdict.within(m["dpx_kp"], m["mad_kp"])
    wm = compat_verdict.within(m["dpx_km"], m["mad_km"])
    return "both" if wp and wm else "poppler" if wp else "mupdf" if wm else "neither"


def validate_attribution(att):
    """Shape, uniqueness, and every label recomputed from its measures:
    a recorded agreement is never trusted."""
    if not isinstance(att, dict) or att.get("schema") != 2 or not isinstance(att.get("pages"), list):
        raise Invalid("attribution: not a schema-2 page list (scripts/compat-attribute.py)")
    _finite_tree(att, "attribution")
    if att.get("tolerance") != compat_verdict.TOLERANCE:
        raise Invalid("attribution: another tolerance than compat_verdict.TOLERANCE")
    cap = att.get("max_pages_per_file", "missing")
    if not compat_verdict.finite(att.get("threshold")) or not (
            cap is None or (isinstance(cap, int) and not isinstance(cap, bool) and cap >= 1)):
        raise Invalid("attribution: no threshold or page cap (a number from 1, or null for every page)")
    t = att.get("truncated", 0)
    if not isinstance(t, int) or isinstance(t, bool) or t < 0:
        raise Invalid("attribution: 'truncated' is not a count")
    seen = set()
    for r in att["pages"]:
        if not isinstance(r, dict) or not isinstance(r.get("id"), str) or not isinstance(r.get("page"), int) \
                or isinstance(r.get("page"), bool) or r["page"] < 1:
            raise Invalid("attribution: a page record without id or page")
        if (r["id"], r["page"]) in seen:
            raise Invalid("attribution: a page recorded twice")
        seen.add((r["id"], r["page"]))
        if r.get("agreement") not in compat_verdict.AGREEMENTS:
            raise Invalid("attribution: unknown agreement")
        if (r["agreement"], r.get("references_agree")) != compat_verdict.verdict(r.get("r300")):
            raise Invalid("attribution: an agreement its measures do not give")
        if (r["agreement"] == "not_comparable") != bool(r.get("reason")):
            raise Invalid("attribution: a reason without not_comparable, or not_comparable without a reason")


def expected_attribution(run, threshold, cap):
    """(id, page) the attribution must hold: every strict-render page
    of an expect: pass file below the threshold — the `cap` worst per
    file when a cap is given (older attributions), all of them when it
    is None (as scripts/compat-attribute.py selects them)."""
    out = set()
    for f in run["files"]:
        if f.get("expect") != "pass":
            continue
        pages = strict_pages(f)
        low = sorted(((v["ssim_raw"], int(pg)) for pg, v in pages.items() if v["ssim_raw"] < threshold))
        out.update((f["id"], pg) for _, pg in (low if cap is None else low[:cap]))
    return out


def strict_pages(f):
    """Page -> (SSIM, content SSIM) of a strict render (none for a
    recovered document or a render that did not pass)."""
    if f.get("recovered") or (f.get("render") or {}).get("class") != "pass" or (f.get("parse") or {}).get("class") != "pass":
        return {}
    out = {}
    for p in (f.get("metrics") or {}).get("render_pages") or []:
        if isinstance(p, dict) and isinstance(p.get("page"), int) and isinstance(p.get("ssim"), (int, float)):
            cs = (p.get("quality") or {}).get("content_ssim")
            out[str(p["page"])] = {"ssim": round(p["ssim"], 4), "ssim_raw": p["ssim"],
                                   "cssim": None if cs is None else round(cs, 4)}
    return out


def attribution_by_file(att):
    out = {}
    for r in (att or {}).get("pages") or []:
        r3 = r.get("r300") or {}
        out.setdefault(r["id"], {})[str(r["page"])] = {
            "agreement": r["agreement"],
            "references_agree": r.get("references_agree"),
            # Per witness: kasane–Poppler (kp) and kasane–MuPDF (km), as
            # measured (compat-attribute quantizes them before its verdict;
            # rounding again could move one across the tolerance).
            **{k: (None if not r3 else r3[k]) for k in ("dpx_kp", "dpx_km", "mad_kp", "mad_km")},
        }
    return out


def summarize(run, attribution=None, env=None):
    gate = load_gate()
    thr = (attribution or {}).get("threshold")
    files = {}
    att = attribution_by_file(attribution) if attribution else {}
    for f in run["files"]:
        rt = f.get("roundtrip") or {}
        files[f["id"]] = {
            "expect": f.get("expect", ""),
            "verdict": f.get("verdict", ""),
            "stages": {s: (f.get(s) or {}).get("class", "") for s in STAGES},
            "causes": sorted(f.get("causes") or []),
            "warnings": len((f.get("metrics") or {}).get("warnings") or []),
            "render_recovered": ((f.get("render_recovered") or {}).get("class") or "skip"),
            "render_approximate": bool(f.get("render_approximate", False)),
            "recovery_notes": int(f.get("recovery_notes", 0) or 0),
            "roundtrip": {c: (rt.get(c) or {}).get("class", "") for c in RT_CHECKS} if rt else {},
            "roundtrip_full": bool(rt.get("full")) if rt else None,
            "incremental": ((f.get("incremental") or {}).get("class") or ""),
            # Semantic round trip per category (absent: not run).
            "semantic": {
                c["category"]: c["class"] for c in ((rt.get("semantic") or {}).get("checks") or [])
            },
            # Report-only semantic categories (PDF 2.0: version, page
            # metadata, associated files, document parts); absent in
            # older reports and baselines.
            "semantic_report": {
                c["category"]: c["class"]
                for c in ((rt.get("semantic") or {}).get("report_only") or [])
            },
            # The declared rules behind each `transformed` category
            # (enforced and report-only); absent when there is none.
            **semantic_rules(rt),
            # rewrite_with(default) against rewrite, its report against the
            # runner's inventory, its version change against T1; absent
            # for an older runner.
            **policy_summary(f),
            # Non-default configurations per file (their own namespace):
            # rewrite class, Full PASS, overall semantic class.
            **options_summary(f),
            # Page-1 text agreement with pdftotext (report only; absent:
            # not compared, or an older runner).
            "text_f1": text_value(f, "text_char_f1"),
            "text_f1_annots": text_value(f, "text_char_f1_with_annots"),
        }
        if env is not None:
            # `below`: under the attribution threshold before rounding —
            # the rounded SSIM can read 0.95 for a page that was below it.
            files[f["id"]]["pages"] = {pg: {**{k: v for k, v in m.items() if k != "ssim_raw"},
                                            **({"below": m["ssim_raw"] < thr} if thr is not None else {})}
                                       for pg, m in strict_pages(f).items()}
        if attribution is not None:
            files[f["id"]]["attribution"] = att.get(f["id"], {})
        cls, n_pages, n_flag, _, _ = quality_detail(f, gate)
        files[f["id"]]["quality"] = cls
        files[f["id"]]["quality_pages"] = n_pages
        files[f["id"]]["quality_pages_flagged"] = n_flag
        files[f["id"]]["quality_report_rules"] = report_rules(f, gate)
    agg = {
        "files": len(files),
        "crash": sum(1 for v in files.values() if v["verdict"] == "crash"),
        "timeout": sum(1 for v in files.values() if v["verdict"] == "timeout"),
    }
    for s in STAGES:
        agg[f"{s}_pass"] = sum(1 for v in files.values() if v["stages"][s] == "pass")
    agg["roundtrip_full"] = sum(1 for v in files.values() if v["roundtrip_full"])
    agg["incremental_pass"] = sum(1 for v in files.values() if v["incremental"] == "pass")
    agg["quality_flag"] = sum(1 for v in files.values() if v["quality"].startswith("flag:"))
    agg["quality_report"] = sum(1 for v in files.values() if v["quality"].startswith("report:"))
    agg["quality_exempt"] = sum(1 for v in files.values() if v["quality"].startswith("exempt:"))
    agg["quality_pages"] = sum(v["quality_pages"] for v in files.values())
    agg["by_config"] = by_config(run)
    out = {"aggregate": agg, "files": files}
    if env is not None:
        meta = {"env": env}
        if attribution is not None:
            meta["attribution"] = {"threshold": attribution.get("threshold"),
                                   "tolerance": attribution.get("tolerance"),
                                   "max_pages_per_file": attribution.get("max_pages_per_file"),
                                   "pages": len(attribution.get("pages") or [])}
            agg["attribution"] = {a: sum(1 for r in attribution["pages"] if r["agreement"] == a)
                                  for a in WITHIN + ("neither", "not_comparable")}
        out["meta"] = meta
    return out


def policy_summary(f):
    pc = f.get("policy_check")
    if not pc:
        return {}
    inv = pc.get("inventory") or {}
    out = {"identical": bool(pc.get("identical"))}
    if inv and not inv.get("runner_error"):
        out["inventory_agree"] = bool(inv.get("agree"))
    if pc.get("version_agrees") is not None:
        out["version_agrees"] = bool(pc["version_agrees"])
    disagree = [
        name for name, o in sorted((f.get("rewrite_options") or {}).items())
        if (o.get("inventory") or {}).get("agree") is False and not (o.get("inventory") or {}).get("runner_error")
    ]
    if disagree:
        out["options_inventory_disagree"] = disagree
    ur = [
        name for name, o in sorted((f.get("rewrite_options") or {}).items())
        if (o.get("usage_rights") or {}).get("agree") is False
    ]
    if ur:
        out["options_usage_rights_disagree"] = ur
    return {"policy": out}


def options_summary(f):
    opts = f.get("rewrite_options") or {}
    if not opts:
        return {}
    strict = lambda s: (f.get(s) or {}).get("class") == "pass" and (s == "parse" or not f.get("recovered"))
    out = {}
    for name, o in sorted(opts.items()):
        rt = o.get("roundtrip") or {}
        ok = (o.get("rewrite") or "").lower() == "pass" and (rt.get("reparse") or {}).get("class", "Pass") == "Pass"
        full = f.get("expect") == "pass" and ok and not f.get("recovered") and all(
            strict(s) for s in ("parse", "render", "text"))
        rec = {"rewrite": (o.get("rewrite") or "").lower(), "full_pass": bool(full),
               "semantic": (rt.get("semantic") or {}).get("class")}
        # Under a structural declaration (T2): what verified it.
        for key in ("structure_declared", "signatures_transformed", "full_declared"):
            if key in rt:
                rec[key] = bool(rt[key])
        report_only = {c["category"]: c["class"] for c in (rt.get("semantic") or {}).get("report_only") or []}
        if "signatures" in report_only:
            rec["signatures"] = report_only["signatures"]
        ur = o.get("usage_rights") or {}
        if ur:
            rec["usage_rights"] = ur.get("outcome")
            if ur.get("sig_flags") is not None:
                rec["sig_flags"] = list(ur["sig_flags"])
        out[name] = rec
    return {"options": out}


def semantic_rules(rt):
    sem = (rt or {}).get("semantic") or {}
    rules = {
        c["category"]: sorted(c.get("rules") or [])
        for c in (sem.get("checks") or []) + (sem.get("report_only") or [])
        if c.get("class") == "transformed"
    }
    return {"semantic_rules": rules} if rules else {}


# Refusal classes of the runner's `configs::Refusal`, by figure.
REFUSALS = {"signature": "signature_refusal", "script": "script_refusal", "multimedia": "multimedia_refusal"}


def config_figures(entries):
    """Figures of one rewrite configuration. `entries` holds, per file,
    (file record, rewrite class, refusal, round trip) under that
    configuration. Full PASS is the enforced definition (expect: pass;
    strict parse, render and text, the rewrite under this configuration
    — never on a recovered document); `full_pass_verified` additionally
    needs the semantic round trip to be pass or transformed and the
    graph comparison equal or exactly the declared change — and, under a
    T2 declaration, the signatures transformed (`signatures_transformed`;
    reports older than C2 do not record it)."""
    fig = {
        k: 0
        for k in (
            "files", "rewrite_pass", "full_pass", "full_pass_verified", "graph_rt", "graph_rt_declared",
            "full_rt", "full_rt_declared", "semantic_pass", "semantic_transformed", "semantic_fail",
            "semantic_error", "semantic_unsupported", "signature_refusal", "script_refusal",
            "multimedia_refusal", "unsupported", "error", "crash_timeout",
            "report_files", "report_script", "report_external", "report_state", "report_navigation",
            "report_multimedia", "report_unknown", "report_unclassified", "inventory_checked",
            "inventory_agree", "inventory_runner_error", "default_identical", "default_differs",
            "version_agrees", "version_differs", "signatures_transformed", "usage_rights_removed",
            "usage_rights_none_present", "usage_rights_refused", "usage_rights_agree",
            "usage_rights_disagree",
        )
    }
    for f, rewrite, refusal, rt, extra in entries:
        fig["files"] += 1
        rt = rt or {}
        if f.get("verdict") in ("crash", "timeout"):
            fig["crash_timeout"] += 1
        ok = rewrite == "pass" and (rt.get("reparse") or {}).get("class", "Pass") == "Pass"
        fig["rewrite_pass"] += ok
        strict = lambda s: (f.get(s) or {}).get("class") == "pass" and (s == "parse" or not f.get("recovered"))
        full = f.get("expect") == "pass" and ok and all(strict(s) for s in ("parse", "render", "text"))
        full = full and not f.get("recovered")
        fig["full_pass"] += full
        graph = (rt.get("structure") or {}).get("class") == "Pass"
        fig["graph_rt"] += graph
        fig["graph_rt_declared"] += bool(rt.get("structure_declared"))
        fig["full_rt"] += bool(rt.get("full"))
        fig["full_rt_declared"] += bool(rt.get("full") or rt.get("full_declared"))
        sem = (rt.get("semantic") or {}).get("class")
        if sem in ("pass", "transformed", "fail", "error", "unsupported"):
            fig[f"semantic_{sem}"] += 1
        declared = bool(rt.get("structure_declared")) and rt.get("signatures_transformed", True) is not False
        fig["signatures_transformed"] += rt.get("signatures_transformed") is True
        ur = (extra or {}).get("usage_rights") if isinstance(extra, dict) else None
        ur_ok = not ur or ur.get("agree") is not False
        if full and sem in ("pass", "transformed", "not_applicable", None) and (graph or declared) and ur_ok:
            fig["full_pass_verified"] += 1
        if ur:
            outcome = ur.get("outcome")
            if outcome in ("removed", "none_present", "refused"):
                fig[f"usage_rights_{outcome}"] += 1
            if ur.get("agree") is True:
                fig["usage_rights_agree"] += 1
            elif ur.get("agree") is False:
                fig["usage_rights_disagree"] += 1
        if refusal in REFUSALS:
            fig[REFUSALS[refusal]] += 1
        elif rewrite == "unsupported":
            fig["unsupported"] += 1
        elif rewrite not in ("pass", "skip", ""):
            fig["error"] += 1
        # The library's report (`rewrite_with`) and its check against the
        # runner's inventory; the default's identity with `rewrite`.
        extra = extra or {}
        rep = extra.get("report")
        if rep:
            fig["report_files"] += 1
            for k in ("script", "external", "state", "navigation", "multimedia", "unknown", "unclassified"):
                fig[f"report_{k}"] += rep.get(k, 0)
        inv = extra.get("inventory")
        if inv:
            fig["inventory_checked"] += 1
            fig["inventory_agree"] += bool(inv.get("agree"))
            fig["inventory_runner_error"] += bool(inv.get("runner_error"))
        if "identical" in extra:
            fig["default_identical" if extra["identical"] else "default_differs"] += 1
        if extra.get("version_agrees") is not None:
            fig["version_agrees" if extra["version_agrees"] else "version_differs"] += 1
    return fig


def by_config(run):
    """Figures per rewrite configuration, each in its own namespace: the
    default from the record's own fields (what the gate enforces), the
    others from `rewrite_options[<name>]`. Nothing here feeds `check`."""
    lower = lambda c: (c or "").lower()
    default, others = [], {}
    for f in run["files"]:
        rt = f.get("roundtrip")
        if rt is None and (f.get("rewrite") or {}).get("class") == "skip":
            continue  # never reached the rewrite (strict parse failed)
        default.append((f, lower((f.get("rewrite") or {}).get("class")), f.get("rewrite_refusal"), rt,
                        f.get("policy_check")))
        for name, o in (f.get("rewrite_options") or {}).items():
            others.setdefault(name, []).append((f, lower(o.get("rewrite")), o.get("refusal"), o.get("roundtrip"), o))
    out = {"default": config_figures(default)}
    for name, entries in sorted(others.items()):
        out[name] = config_figures(entries)
    return out


def text_value(f, key):
    v = (f.get("metrics") or {}).get(key)
    return None if v is None else round(v, 4)


# A page-1 F1 drop at least this large is listed by `check` for review
# (report only: the text gate is not enforced per file).
TEXT_F1_DROP = 0.05


# Semantic classes that are not a success.
SEMANTIC_BAD = ("fail", "error", "unsupported")


def semantic_moved(was, now):
    """A semantic class change to list for review (never a failure of
    the gate): a pass or a transformation lost, or a move into a class
    that is not a success from any other class (`not_applicable` ->
    `fail` is a structure the rewrite added). A move *to* `transformed`
    is listed with its rules elsewhere. `transformed` -> `pass` is not
    listed: the rules are derived again from the input on every run, so
    a declared change that stops happening is a `fail`, and a `pass` is
    the strictest outcome."""
    if now == was or now == "transformed":
        return False
    if was in ("pass", "transformed"):
        return now != "pass"
    return now in SEMANTIC_BAD


def worse(new, old):
    return RANK.get(new, 9) > RANK.get(old, 9)


WITNESSES = (("Poppler", "kp"), ("MuPDF", "km"))
# The mark of a first measurement with no witness result in the baseline
# (update-baseline --accept-unverified accepts these and nothing else).
UNVERIFIED = " — unverified"


def attribution_usable(meta, env, attribution):
    """(regressions, usable): whether the run's attribution can be judged
    against the baseline's — the same environment and commit as the run,
    the same threshold and tolerance, at least the baseline's coverage,
    nothing truncated. Decided once, for the check and its ledger."""
    regs = []
    if not meta.get("attribution"):
        return regs, False
    if attribution is None:
        return ["attribution not measured (the baseline has it; --attribution)"], False
    aenv = attribution.get("env") or {}
    gaps = [f"{side} {k}" for side, e in (("run", env or {}), ("attribution", aenv))
            for k in compat_env.COMPARED + ("commit",) if k != "features" and not e.get(k)]
    if gaps:
        regs.append("environment incomplete: " + ", ".join(gaps) + " (the same commit cannot be established)")
    d = compat_env.differences(env or {}, aenv, keys=compat_env.COMPARED + ("commit",))
    if d:
        regs.append("attribution measured in another environment or commit than the run: " + ", ".join(d))
    for k in ("threshold", "tolerance"):
        if attribution.get(k) != meta["attribution"].get(k):
            regs.append(f"attribution {k} differs from the baseline's")
    run_cap = attribution.get("max_pages_per_file")
    if run_cap is not None:
        # Whatever the baseline's own coverage: a page beyond a cap would
        # be below the threshold and never judged.
        regs.append(f"attribution capped at {run_cap} page(s) per file: every page below the threshold is "
                    "required")
    if attribution.get("truncated"):
        regs.append(f"attribution incomplete: {attribution['truncated']} page(s) below the threshold "
                    "not measured (--max-pages-total)")
    # A page whose PDF was missing was not measured (compat-attribute
    # exits 2 on it): never equal to a baseline that has the same gap.
    gone = sum(1 for p in attribution.get("pages") or [] if p.get("reason") == "not_fetched")
    if gone:
        regs.append(f"attribution incomplete: {gone} page(s) not measured, their PDF missing (not_fetched)")
    return regs, not regs


def _within(a, k):
    return compat_verdict.within(a.get(f"dpx_{k}"), a.get(f"mad_{k}"))


def attribution_ledger(run, cur, old, meta, attribution, reviewed=None):
    """One record per (case, page) that the baseline attributes, the run
    attributes, or the run has below the attribution threshold (M2):
    which side has the page, whether it is a baseline target, a new
    candidate, why it is measured, whether it is comparable — and the
    decisions taken on it. Pages are identified by (case id, page
    number) only, never by their rank or position. `attribution` is None
    when the run's attribution cannot be used (another environment,
    commit, threshold; fewer pages than the baseline's; truncated): then
    every page that needed it is recorded as not verified, and the
    reason is already a regression."""
    if not meta.get("attribution"):
        return []
    thr = meta["attribution"].get("threshold")
    base_cap = meta["attribution"].get("max_pages_per_file")
    att = attribution_by_file(attribution) if attribution is not None else {}
    want = expected_attribution(run, attribution["threshold"], attribution.get("max_pages_per_file")) \
        if attribution is not None else set()
    run_low = {(f["id"], int(pg)) for f in run["files"] if f.get("expect") == "pass"
               for pg, v in strict_pages(f).items() if thr is not None and v["ssim_raw"] < thr}
    keys = set(run_low) | {(fid, int(pg)) for fid, pages in att.items() for pg in pages}
    for fid, o in old.items():
        keys |= {(fid, int(pg)) for pg in (o.get("attribution") or {})}
    out = []
    for fid, pg in sorted(keys):
        o, n, key = old.get(fid), cur.get(fid), str(pg)
        ov = ((o or {}).get("pages") or {}).get(key)
        nv = ((n or {}).get("pages") or {}).get(key)
        oa = ((o or {}).get("attribution") or {}).get(key)
        na = att.get(fid, {}).get(key)
        base_attr = (o or {}).get("attribution") or {}
        if ov is None:
            base_low = None
        elif "below" in ov:
            base_low = ov["below"]
        else:  # an older baseline: the rounded SSIM (0.95 exactly reads as not below — the safe side)
            base_low = thr is not None and ov["ssim"] < thr
        e = {"id": fid, "page": pg,
             "in_baseline": ov is not None or oa is not None, "in_run": nv is not None,
             "baseline_target": oa is not None, "below_threshold": (fid, pg) in run_low,
             "new_candidate": oa is None and (fid, pg) in run_low,
             "selected_because": ("baseline target" if oa is not None else
                                  "below the threshold" if (fid, pg) in run_low else "attributed"),
             "attributed": na is not None, "comparable": na is not None and na["agreement"] != "not_comparable",
             "reason": None, "regressions": [], "improvements": [], "notes": [], "unverified": [],
             "accepted": []}
        reg, imp, note = e["regressions"].append, e["improvements"].append, e["notes"].append
        out.append(e)
        if n is None:
            e["reason"] = "case missing from the run"  # a regression of its own (stages)
            continue
        if attribution is None:
            e["reason"] = "attribution not usable"
            continue
        if na is None:
            if (fid, pg) in want:
                e["reason"] = "not attributed"
                reg(f"{fid}: page {pg} not attributed (below the attribution threshold; the baseline attributes it)"
                    if oa is not None else f"{fid}: page {pg} below the attribution threshold but not attributed")
            elif oa is not None and nv is None:
                e["reason"] = "page missing from the run"
                if ov is None:  # otherwise reported as no longer compared
                    reg(f"{fid}: page {pg} attributed in the baseline, missing from the run")
            elif oa is not None and (fid, pg) not in run_low:
                e["reason"] = "no longer below the threshold"
                imp(f"{fid}: page {pg} no longer below the attribution threshold")
            elif oa is not None:
                e["reason"] = "not attributed (capped run)"
                reg(f"{fid}: page {pg} not attributed (the baseline attributes it)")
            continue
        if (fid, pg) not in want:
            e["reason"] = "attributed but not below the threshold"
            reg(f"{fid}: page {pg} attributed but not among the run's pages below the threshold")
            continue
        if na["agreement"] == "not_comparable":
            e["reason"] = "not comparable"
        refs = " — the references agree, kasane alone differs" if na.get("references_agree") else ""
        if oa is not None:
            was, now = oa["agreement"], na["agreement"]
            if was != "not_comparable" and now == "not_comparable":
                reg(f"{fid}: page {pg} agreement {was} -> not_comparable"
                    + (" (an open page hidden)" if was == "neither" else ""))
                continue
            if was == "not_comparable":
                if now != "not_comparable":
                    note(f"{fid}: page {pg} comparable again: {now}")
                    if now not in WITHIN:
                        reg(f"{fid}: page {pg} not_comparable -> {now}{refs}")
                continue
            # Each witness on its own: leaving either's tolerance is a
            # regression whatever the other does — gaining one never
            # offsets losing the other.
            lost = [name for name, k in WITNESSES if _within(oa, k) and not _within(na, k)]
            gained = [name for name, k in WITNESSES if not _within(oa, k) and _within(na, k)]
            if lost:
                reg(f"{fid}: page {pg} agreement {was} -> {now} (outside {' and '.join(lost)}'s tolerance now{refs})")
            elif gained:
                imp(f"{fid}: page {pg} agreement {was} -> {now} (within {' and '.join(gained)}'s tolerance now)")
            if was == "neither" and now == "neither":
                # An open page: growing away from either witness is a
                # change of kasane's render.
                for name, k in WITNESSES:
                    grew = [m for m, lim in ((f"dpx_{k}", OPEN_DPX_GROWTH), (f"mad_{k}", OPEN_MAD_GROWTH))
                            if na[m] - oa[m] > lim]
                    if reviewed is not None:
                        grew = [m for m in grew if not reviewed.accepts(
                            fid, pg, m, oa[m], na[m],
                            f"{fid}: open page {pg} differs more from {name} ({m} {oa[m]} -> {na[m]})",
                            e["accepted"])]
                    if grew:
                        reg(f"{fid}: open page {pg} differs more from {name} ("
                            + ", ".join(f"{m} {oa[m]} -> {na[m]}" for m in grew) + ")")
            continue
        # Not attributed in the baseline: a new candidate.
        if o is None:
            if na["agreement"] not in WITHIN:
                reg(f"{fid}: page {pg} {na['agreement']} (file not in the baseline){refs}")
            continue
        if ov is not None and base_low and (base_cap is None or len(base_attr) < base_cap):
            # Below the threshold in the baseline, which had room for it:
            # the baseline cannot vouch for this page at all.
            reg(f"{fid}: page {pg} {na['agreement']}: below the threshold in the baseline, which does not "
                f"attribute it (the baseline is incomplete){refs}")
            continue
        first = ov is not None and base_low  # below the threshold before, beyond the old cap
        if na["agreement"] == "both" or (na["agreement"] in WITHIN and not first):
            note(f"{fid}: page {pg} newly attributed: {na['agreement']}")
            continue
        if not first:
            what = "a page the baseline does not have" if ov is None else f"SSIM {ov['ssim']} -> {nv['ssim']}"
            reg(f"{fid}: page {pg} newly {na['agreement']} (below the threshold, not in the baseline: {what}){refs}")
        else:
            # Below the threshold in the baseline too, beyond an older
            # baseline's cap: its witness result then is unknown, so
            # anything short of `both` now cannot be told from a lost
            # witness — unverified, not passed. A baseline that attributes
            # every page ends it.
            msg = (f"{fid}: page {pg} first attributed: {na['agreement']}{UNVERIFIED}: below the threshold in "
                   f"the baseline too, beyond its {base_cap} worst per file, so no witness result to compare "
                   f"(update the baseline to one that attributes every page){refs}")
            reg(msg)
            if na["agreement"] != "not_comparable":
                # A measured page: the migration may accept it. A page
                # that cannot be measured stays a regression to decide on.
                e["unverified"].append(msg)
    return out


def numeric_regressions(run, cur, old, base, env, attribution, require_same_env, improvements, notes,
                        unverified=None, reviewed=None):
    """M1-c: the numeric comparisons, each page judged on its own — an
    improvement elsewhere never offsets a regression."""
    regs = []
    meta = base.get("meta") or {}
    benv = meta.get("env")
    if benv is None:
        msg = "numeric measures not compared: the baseline records none (update-baseline arms them)"
        (regs if require_same_env else notes).append(msg)
        return regs
    if env is None:
        msg = "numeric measures not compared: this run's environment was not given (--env)"
        (regs if require_same_env else notes).append(msg)
        return regs
    diff = compat_env.differences(benv, env)
    if diff:
        msg = ("numeric measures not compared: the environment differs from the baseline's in "
               + ", ".join(f"{k} ({benv.get(k)} -> {env.get(k)})" for k in diff))
        (regs if require_same_env else notes).append(msg)
        return regs
    att_regs, usable = attribution_usable(meta, env, attribution)
    regs.extend(att_regs)
    ledger = attribution_ledger(run, cur, old, meta, attribution if usable else None, reviewed)
    for e in ledger:
        regs.extend(e["regressions"])
        if unverified is not None:
            unverified.extend(e["unverified"])
        improvements.extend(e["improvements"])
        notes.extend(e["notes"])
    for fid, o in old.items():
        n = cur.get(fid)
        if n is None:
            continue
        op, np_ = o.get("pages"), n.get("pages") or {}
        if op is not None:
            for pg, ov in op.items():
                nv = np_.get(pg)
                if nv is None:
                    regs.append(f"{fid}: page {pg} no longer compared with the reference")
                    continue
                if nv["ssim"] < ov["ssim"] - SSIM_DROP:
                    regs.append(f"{fid}: page {pg} SSIM {ov['ssim']} -> {nv['ssim']}")
                elif nv["ssim"] > ov["ssim"] + SSIM_DROP:
                    improvements.append(f"{fid}: page {pg} SSIM {ov['ssim']} -> {nv['ssim']}")
                if ov.get("cssim") is not None:
                    if nv.get("cssim") is None:
                        regs.append(f"{fid}: page {pg} content SSIM no longer measured")
                    elif nv["cssim"] < ov["cssim"] - CONTENT_SSIM_DROP:
                        line = f"{fid}: page {pg} content SSIM {ov['cssim']} -> {nv['cssim']}"
                        if reviewed is None or not reviewed.accepts(fid, int(pg), "cssim", ov["cssim"],
                                                                     nv["cssim"], line):
                            regs.append(line)
            for key in ("text_f1", "text_f1_annots"):
                was, now = o.get(key), n.get(key)
                if was is None:
                    continue
                if now is None:
                    regs.append(f"{fid}: {key} no longer measured (was {was})")
                elif now < was - TEXT_F1_DROP_ENFORCED:
                    regs.append(f"{fid}: {key} {was} -> {now}")
    return regs


def check(run, base, summary_md=None, env=None, attribution=None, require_same_env=False, ledger_path=None,
          collect=None, collect_unverified=None, reviewed_differences=None):
    validate_run(run)
    validate_base(base)
    if attribution is not None:
        validate_attribution(attribution)
    gate = load_gate()
    cur = summarize(run, attribution, env if env is not None else None)["files"]
    old = base["files"]
    notes = []
    armed = any("quality" in o for o in old.values())
    regressions, improvements, known_flags, unrecorded = [], [], [], []
    new_flags = []
    new_reports, known_reports = [], []
    semantic_changes = []
    option_changes = []
    not_run = set()
    not_in_baseline = set()
    text_changes = []
    for fid, n in cur.items():
        if n["verdict"] in ("crash", "timeout"):
            regressions.append(f"{fid}: verdict {n['verdict']}")
        o = old.get(fid)
        if o is None:
            if armed and n["quality"].startswith("flag:"):
                new_flags.append(fid)
                regressions.append(f"{fid}: exact render {n['quality']} (quality gate; file not in the baseline)")
            continue
        for s in STAGES:
            if o["stages"][s] == "pass" and n["stages"][s] != "pass":
                regressions.append(f"{fid}: {s} pass -> {n['stages'][s]}")
            elif worse(n["stages"][s], o["stages"][s]):
                regressions.append(f"{fid}: {s} {o['stages'][s]} -> {n['stages'][s]}")
            elif n["stages"][s] == "pass" and o["stages"][s] != "pass":
                improvements.append(f"{fid}: {s} {o['stages'][s]} -> pass")
        if n.get("warnings", 0) and not o.get("warnings", 0) and n["verdict"] == "pass":
            print(f"note: {fid}: now accepted with warnings (was a clean pass)")
        if (
            n.get("render_recovered") == "pass"
            and o.get("render_recovered", "skip") != "pass"
            and n.get("render") != "pass"
        ):
            print(f"note: {fid}: now renders in recovery mode (not counted as strict)")
        if n.get("render_approximate") and not o.get("render_approximate") and n.get("render") == "pass":
            print(f"note: {fid}: strict render now uses a reported approximation")
        for c in RT_CHECKS:
            oc, nc = o["roundtrip"].get(c, ""), n["roundtrip"].get(c, "")
            if oc == "Pass" and nc == "Fail":
                regressions.append(f"{fid}: round-trip {c} PASS -> FAIL")
        # Semantic round trip: reported, not yet enforced (no baseline
        # from the nightly configuration so far).
        for cat, was in (o.get("semantic") or {}).items():
            now = (n.get("semantic") or {}).get(cat)
            if now is None and n.get("semantic"):
                # The run has a semantic record without this category.
                semantic_changes.append(f"{fid}: semantic {cat} {was} -> missing")
            elif now is not None and semantic_moved(was, now):
                semantic_changes.append(f"{fid}: semantic {cat} {was} -> {now}")
        # Report-only categories: the same, or — when the baseline
        # predates them — any outcome that is not a pass or not
        # applicable. Never enforced.
        old_report = o.get("semantic_report")
        for cat, now in (n.get("semantic_report") or {}).items():
            was = (old_report or {}).get(cat)
            if old_report is None or was is None:
                if now not in ("pass", "not_applicable", "transformed"):
                    semantic_changes.append(f"{fid}: report-only semantic {cat}: {now} (no baseline)")
            elif semantic_moved(was, now):
                semantic_changes.append(f"{fid}: report-only semantic {cat} {was} -> {now}")
        # A declared transformation is never a pass: every category that
        # is `transformed` and was not, or under other rules, is listed.
        old_rules = o.get("semantic_rules") or {}
        for cat, rules in (n.get("semantic_rules") or {}).items():
            was = (o.get("semantic") or {}).get(cat) or (o.get("semantic_report") or {}).get(cat)
            if was != "transformed" or old_rules.get(cat) != rules:
                semantic_changes.append(
                    f"{fid}: semantic {cat} {was or 'unrecorded'} -> transformed ({', '.join(rules)})"
                )
        # Text agreement: listed here; enforced among the numeric
        # measures (same environment, a baseline with page measures).
        for key in ("text_f1", "text_f1_annots"):
            was, now = o.get(key), n.get(key)
            if was is not None and (now is None or now <= was - TEXT_F1_DROP):
                text_changes.append(f"{fid}: {key} {was} -> {now}")
        # The default's identity with `rewrite`, its report against the
        # runner's inventory and its version change: enforced (absolute,
        # not against the baseline). Other configurations: listed.
        pol = n.get("policy") or {}
        if pol.get("identical") is False:
            regressions.append(f"{fid}: rewrite_with(default) differs from rewrite")
        if pol.get("inventory_agree") is False:
            regressions.append(f"{fid}: the default's report disagrees with the runner's inventory")
        if pol.get("version_agrees") is False:
            regressions.append(f"{fid}: the default's version change disagrees with the runner's T1")
        for name in pol.get("options_inventory_disagree") or []:
            semantic_changes.append(f"{fid}: {name}: the report disagrees with the runner's inventory")
        for name in pol.get("options_usage_rights_disagree") or []:
            semantic_changes.append(f"{fid}: {name}: the usage-rights report disagrees with the runner's reading")
        # A configuration's lost gain (its own namespace; listed, never
        # enforced): Full PASS, rewrite, semantic pass.
        if o.get("policy") and not n.get("policy") and n["stages"].get("parse") == "pass":
            option_changes.append(f"{fid}: policy check not run (the baseline has one)")
        for name, was_o in (o.get("options") or {}).items():
            now_o = (n.get("options") or {}).get(name)
            if now_o is None:
                not_run.add(name)
                continue
            for key, good in (("full_pass", True), ("rewrite", "pass"), ("semantic", "pass"),
                              ("structure_declared", True), ("signatures_transformed", True),
                              ("full_declared", True), ("signatures", "transformed"),
                              ("usage_rights", "removed")):
                if was_o.get(key) == good and now_o.get(key) != good:
                    option_changes.append(f"{fid}: {name}: {key} {was_o.get(key)} -> {now_o.get(key)}")
            if "sig_flags" in was_o and now_o.get("sig_flags") != was_o["sig_flags"]:
                option_changes.append(
                    f"{fid}: {name}: /SigFlags change {was_o['sig_flags']} -> {now_o.get('sig_flags')}")
        for name in (n.get("options") or {}):
            if name not in (o.get("options") or {}) and o:
                not_in_baseline.add(name)
        if o.get("incremental") == "pass" and n.get("incremental") not in ("pass", None, ""):
            regressions.append(f"{fid}: incremental pass -> {n['incremental']}")
        oq, nq = o.get("quality", ""), n["quality"]
        if nq.startswith("flag:") and "quality" not in o:
            # Baseline from before the gate: nothing to compare with yet
            # (staged roll-out — regenerate the baseline to arm it).
            unrecorded.append(f"{fid}: {nq}")
        elif nq.startswith("flag:"):
            if oq.startswith("flag:"):
                known_flags.append(f"{fid}: {nq}")
                was, now = o.get("quality_pages_flagged", 0), n["quality_pages_flagged"]
                if was and now > was:
                    new_flags.append(fid)
                    regressions.append(f"{fid}: {now} flagged pages, {was} in the baseline (quality gate)")
            else:
                new_flags.append(fid)
                regressions.append(f"{fid}: exact render {nq} (quality gate; was {oq or 'unrecorded'})")
        elif oq.startswith("flag:"):
            improvements.append(f"{fid}: quality {oq} -> {nq}")
        # Report-only findings never fail; new ones (a rule the baseline
        # does not record for this file) are listed for review.
        if "quality_report_rules" in o:
            fresh = sorted(set(n["quality_report_rules"]) - set(o["quality_report_rules"]))
            if fresh:
                new_reports.append(f"{fid}: {','.join(fresh)}")
            elif n["quality_report_rules"]:
                known_reports.append(fid)
    reviewed = gate.get("reviewed_known_flags", 0)
    in_baseline = sum(1 for o in old.values() if o.get("quality", "").startswith("flag:"))
    if armed and in_baseline > reviewed:
        regressions.append(
            f"baseline records {in_baseline} quality-flagged files, more than the "
            f"{reviewed} reviewed in compat/quality-gate.json (reviewed_known_flags)"
        )
    if summary_md:
        write_gate_summary(
            summary_md, run, gate, armed, known_flags, new_flags, unrecorded, in_baseline, reviewed,
            new_reports, known_reports,
        )
    missing = sorted(set(old) - set(cur))
    added = sorted(set(cur) - set(old))
    for fid in missing:
        regressions.append(f"{fid}: in the baseline, missing from the run")
    entries = load_reviewed() if reviewed_differences is None else reviewed_differences
    shas = {f["id"]: f.get("sha256") for f in run["files"]}
    if ledger_path:
        meta = base.get("meta") or {}
        benv = meta.get("env")
        same = benv is not None and env is not None and not compat_env.differences(benv, env)
        usable = same and attribution_usable(meta, env, attribution)[1]
        led = attribution_ledger(run, cur, old, meta, attribution if usable else None, Reviewed(entries, env, shas))
        with open(ledger_path, "w") as fh:
            json.dump([{k: v for k, v in e.items()
                        if k not in ("regressions", "improvements", "notes", "unverified", "accepted")}
                       | {"decisions": e["regressions"] + [f"ACCEPTED (reviewed, limited): {a}" for a in e["accepted"]]
                          + e["improvements"] + e["notes"]} for e in led],
                      fh, indent=1, sort_keys=True)
            fh.write("\n")
    rev = Reviewed(entries, env, shas)
    regressions.extend(numeric_regressions(run, cur, old, base, env, attribution, require_same_env, improvements,
                                           notes, collect_unverified, rev))
    print(f"files: {len(cur)} now, {len(old)} in baseline, {len(missing)} missing, {len(added)} new")
    for line in notes:
        print("note:", line)
    for line in improvements:
        print("improved:", line)
    for line in known_flags:
        print("known quality flag (in baseline):", line)
    for line in unrecorded:
        print("quality flag (baseline predates the gate, not enforced):", line)
    for line in new_reports:
        print("new report-only finding (not enforced, review):", line)
    for line in semantic_changes:
        print("semantic round-trip change (not enforced, review):", line)
    for line in text_changes:
        print("text agreement drop (not enforced, review):", line)
    for name in sorted(not_run):
        print("rewrite configuration not run:", name, "(the baseline has it; not enforced)")
    for name in sorted(not_in_baseline - not_run):
        print("rewrite configuration not in the baseline:", name, "(nothing to compare; not enforced)")
    for line in option_changes:
        print("rewrite configuration change (not enforced, review):", line)
    if collect is not None:
        # A baseline update decides on the original rule: a reviewed
        # difference is still a regression it must accept explicitly.
        collect.extend(regressions + rev.original)
    for line in rev.accepted:
        print("regression under the original rule, ACCEPTED (reviewed, limited):", line)
    for line in rev.refused:
        print("note:", line)
    for line in regressions:
        print("REGRESSION:", line)
    if rev.accepted:
        print(f"{len(rev.accepted)} regression(s) under the original rule accepted as reviewed differences")
    if regressions:
        print(f"{len(regressions)} regression(s)")
        return 1
    print("no regressions")
    return 0


def write_gate_summary(
    path, run, gate, armed, known, new, unrecorded, in_baseline, reviewed, new_reports=(), known_reports=()
):
    """Counts-only markdown for the CI job summary."""
    import collections

    c = collections.Counter()
    pages = flagged = reported = exempt = exact = 0
    for f in run["files"]:
        cls, n, nf, nr, ne = quality_detail(f, gate)
        c[cls.split(":")[0]] += 1
        exact += is_exact(f)
        pages, flagged, reported, exempt = pages + n, flagged + nf, reported + nr, exempt + ne
    per_rule = collections.Counter()
    for f in run["files"]:
        if not is_exact(f):
            continue
        for p in (f.get("metrics") or {}).get("render_pages") or []:
            if p.get("quality"):
                per_rule.update(page_findings(p["quality"], gate)[0])
    if not armed:
        result = "not enforced (baseline predates the gate)"
    elif new or in_baseline > reviewed:
        result = "**FAIL**"
    else:
        result = "pass"
    lo, hi = gate["rules"]["ink_ratio"]["low"], gate["rules"]["ink_ratio"]["high"]
    rows = [
        ("exact renders (files)", exact),
        ("exact files compared with Poppler", c["ok"] + c["flag"] + c["report"] + c["exempt"]),
        ("pages compared", pages),
        (f"files flagged by an enforced rule (ink ratio outside [{lo}, {hi}])", c["flag"]),
        ("pages flagged by an enforced rule", flagged),
        ("files with report-only findings", c["report"]),
        ("pages with report-only findings", reported),
        ("files exempt (blank reference / page size)", c["exempt"]),
        ("pages exempt", exempt),
        ("new flags (fail)", len(new)),
        ("new report-only findings (review, not enforced)", len(new_reports)),
        ("report-only findings already in the baseline", len(known_reports)),
        ("known flags (in the baseline)", len(known)),
        ("known flags reviewed in quality-gate.json", reviewed),
        ("flags not enforced (baseline predates the gate)", len(unrecorded)),
    ] + [
        (f"pages: `{name}` ({'enforced' if r['enforced'] else 'report only'})", per_rule[name])
        for name, r in gate["rules"].items()
    ]
    with open(path, "a") as fh:
        fh.write(f"## Exact-render quality gate: {result}\n\n| | count |\n|---|---:|\n")
        for k, v in rows:
            fh.write(f"| {k} | {v} |\n")
        fh.write("\nRules and thresholds: `compat/quality-gate.json`, `docs/plans/quality-gate.md`.\n\n")


def _no_duplicate_keys(pairs):
    out = {}
    for k, v in pairs:
        if k in out:
            raise ValueError(f"key {k!r} twice")
        out[k] = v
    return out


def load(path, what):
    try:
        with open(path) as fh:
            return json.load(fh, object_pairs_hook=_no_duplicate_keys)
    except (OSError, ValueError) as e:
        raise Invalid(f"{what} {path}: {e}")


def baseline_diff(new, old):
    """What replacing `old` by `new` changes: {category: [file ids]}."""
    d = {}
    add = lambda k, fid: d.setdefault(k, []).append(fid)
    of, nf = (old or {}).get("files") or {}, new["files"]
    for fid in sorted(set(of) - set(nf)):
        add("removed files", fid)
    for fid in sorted(set(nf) - set(of)):
        add("added files", fid)
    for fid in sorted(set(of) & set(nf)):
        o, n = of[fid], nf[fid]
        for s in STAGES:
            if o["stages"].get(s) != n["stages"].get(s):
                add(f"{s} {o['stages'].get(s)} -> {n['stages'].get(s)}", fid)
        if o.get("quality") != n.get("quality"):
            add(f"quality {str(o.get('quality')).split(':')[0]} -> {str(n.get('quality')).split(':')[0]}", fid)
        op, np_ = o.get("pages"), n.get("pages") or {}
        if op is None and np_:
            add("page measures recorded (new)", fid)
        for pg, ov in (op or {}).items():
            nv = np_.get(pg)
            if nv is None:
                add("pages no longer compared", fid)
            elif nv["ssim"] < ov["ssim"] - SSIM_DROP:
                add("page SSIM lower", fid)
            elif nv["ssim"] > ov["ssim"] + SSIM_DROP:
                add("page SSIM higher", fid)
        oa, na = o.get("attribution"), n.get("attribution") or {}
        for pg, b in na.items():
            was = (oa or {}).get(pg, {}).get("agreement", "unrecorded")
            if was != b["agreement"]:
                add(f"agreement {was} -> {b['agreement']}", fid)
        for pg in (oa or {}):
            if pg not in na:
                add("pages no longer attributed", fid)
    om, nm = ((old or {}).get("meta") or {}).get("attribution") or {}, (new.get("meta") or {}).get("attribution") or {}
    if om and nm and om.get("max_pages_per_file") != nm.get("max_pages_per_file"):
        d["attribution pages per file: " + " -> ".join(
            "every page" if c is None else str(c) for c in (om.get("max_pages_per_file"), nm.get("max_pages_per_file")))] = []
    return d


def update_baseline(a):
    run = load(a.run, "run report")
    validate_run(run)
    env = load(a.env, "environment")
    att = load(a.attribution, "attribution") if a.attribution else None
    if att is not None:
        validate_attribution(att)
        d = compat_env.differences(env, att.get("env") or {}, keys=compat_env.COMPARED + ("commit",))
        if d:
            raise Invalid("the attribution was measured in another environment or commit: " + ", ".join(d))
    missing = compat_env.incomplete(env, need_mupdf=att is not None)
    if missing:
        raise Invalid("environment incomplete: " + ", ".join(missing))
    if att is not None:
        if att.get("max_pages_per_file") is not None:
            raise Invalid(f"the attribution is capped at {att['max_pages_per_file']} page(s) per file; a "
                          "baseline records every page below the threshold")
        if att.get("truncated"):
            raise Invalid(f"the attribution is truncated ({att['truncated']} page(s) not measured)")
        gone = sum(1 for p in att.get("pages") or [] if p.get("reason") == "not_fetched")
        if gone:
            raise Invalid(f"the attribution has {gone} page(s) not measured, their PDF missing (not_fetched)")
        have = {(r["id"], r["page"]) for r in att["pages"]}
        want = expected_attribution(run, att["threshold"], att.get("max_pages_per_file"))
        if have != want:
            raise Invalid(f"the attribution does not cover the run's pages below its threshold "
                          f"({len(want - have)} missing, {len(have - want)} extra)")
    old = None
    if os.path.exists(a.baseline):
        old = load(a.baseline, "baseline")
        validate_base(old)
    new = summarize(run, att, env)
    diff = baseline_diff(new, old)
    ref = env.get("reference") or {}
    print(f"update-baseline: {a.baseline} ({'new file' if old is None else 'replacing'}); environment "
          + ", ".join(f"{k} {env.get(k)}" for k in ("commit", "os", "arch", "poppler", "mupdf", "features"))
          + f", reference {ref.get('profile')} v{ref.get('version')} (config {str(ref.get('config_sha256'))[:12]}, "
          f"fonts {str(ref.get('fonts_sha256'))[:12]})")
    if old is not None and (old.get("meta") or {}).get("env"):
        changed = compat_env.differences(old["meta"]["env"], env)
        print("environment change: " + (", ".join(changed) if changed else "none"))
    for k, ids in sorted(diff.items()):
        print(f"  {k}: {len(ids)}" + (f"  [{', '.join(ids)}]" if a.ids else ""))
    if not diff:
        print("  no file changes")
    regressions, found, unverified = 0, [], []
    if old is not None:
        print("--- check of this run against the current baseline:")
        regressions = check(run, old, env=env, attribution=att, require_same_env=True, collect=found,
                            collect_unverified=unverified)
    if not a.write:
        print("dry run: nothing written (--write writes it)")
        return 0
    if diff.get("removed files") and not a.allow_removed:
        print(f"refused: {len(diff['removed files'])} file(s) would be removed (--allow-removed)")
        return 1
    # By the ledger's record of each, never by the text.
    others = list(found)
    for r in unverified:
        others.remove(r)
    if others and not a.accept_regressions:
        print(f"refused: the run has regressions against the current baseline ({len(others)}; "
              "--accept-regressions after reviewing them)")
        return 1
    if unverified and not (a.accept_unverified or a.accept_regressions):
        print(f"refused: {len(unverified)} page(s) measured for the first time are unverified "
              "(--accept-unverified, the M2 migration, after reviewing them)")
        return 1
    if unverified and a.accept_unverified and not others:
        print(f"accepted: {len(unverified)} first measurement(s) of pages below the threshold in the old baseline")
    with open(a.baseline, "w") as fh:
        json.dump(new, fh, indent=1, sort_keys=True)
        fh.write("\n")
    print(f"written: {a.baseline}")
    return 0


def main(argv):
    ap = argparse.ArgumentParser(prog="ci-regression.py", description="Regression gate over a kasane-compat report.")
    sub = ap.add_subparsers(dest="cmd")
    p = sub.add_parser("summarize")
    p.add_argument("run")
    p.add_argument("--env")
    p.add_argument("--attribution")
    p = sub.add_parser("check")
    p.add_argument("run")
    p.add_argument("baseline")
    p.add_argument("--summary-md")
    p.add_argument("--env")
    p.add_argument("--attribution")
    p.add_argument("--require-same-env", action="store_true")
    p.add_argument("--ledger", help="write the per-page attribution ledger (ids and decisions) here")
    p = sub.add_parser("quality")
    p.add_argument("run")
    p = sub.add_parser("env")
    p.add_argument("--commit")
    p.add_argument("--features", default="")
    p = sub.add_parser("update-baseline")
    p.add_argument("run")
    p.add_argument("baseline")
    p.add_argument("--env", required=True)
    p.add_argument("--attribution")
    p.add_argument("--write", action="store_true")
    p.add_argument("--allow-removed", action="store_true")
    p.add_argument("--accept-regressions", action="store_true")
    p.add_argument("--accept-unverified", action="store_true",
                   help="accept the first measurements of pages an older baseline did not attribute (M2), "
                        "and no other regression")
    p.add_argument("--ids", action="store_true")
    try:
        a = ap.parse_args(argv[1:])
    except SystemExit:
        return 2
    try:
        if a.cmd == "summarize":
            run = load(a.run, "run report")
            validate_run(run)
            env = load(a.env, "environment") if a.env else None
            att = load(a.attribution, "attribution") if a.attribution else None
            if att is not None:
                validate_attribution(att)
            json.dump(summarize(run, att, env), sys.stdout, indent=1, sort_keys=True)
            sys.stdout.write("\n")
            return 0
        if a.cmd == "check":
            run, base = load(a.run, "run report"), load(a.baseline, "baseline")
            env = load(a.env, "environment") if a.env else None
            att = load(a.attribution, "attribution") if a.attribution else None
            return check(run, base, a.summary_md, env=env, attribution=att, require_same_env=a.require_same_env,
                         ledger_path=a.ledger)
        if a.cmd == "env":
            json.dump(compat_env.environment(commit=a.commit, features=a.features), sys.stdout, indent=1,
                      sort_keys=True)
            sys.stdout.write("\n")
            return 0
        if a.cmd == "update-baseline":
            return update_baseline(a)
        if a.cmd == "quality":
            run = load(a.run, "run report")
            gate = load_gate()
            import collections
            c = collections.Counter()
            for f in run["files"]:
                q = quality(f, gate)
                c[q.split(":")[0]] += 1
                if q.startswith(("flag:", "report:", "exempt:")):
                    print(f"{q}\t{f['id']}")
            print("summary:", dict(c))
            return 0
    except Invalid as e:
        print(f"INVALID: {e}")
        return 2
    sys.stderr.write(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
