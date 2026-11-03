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

Only aggregates and cause ids are ever written; the corpus itself and
rendered images stay out of git.
"""
import json
import os
import sys

STAGES = ("parse", "render", "text", "rewrite")
RT_CHECKS = ("reparse", "structure", "render", "text", "annotations", "external_valid", "external_no_new")
RANK = {"pass": 0, "skip": 0, "mismatch": 1, "unsupported": 2, "error": 3, "timeout": 4, "crash": 5}


GATE_CONFIG = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "compat", "quality-gate.json")


def load_gate():
    with open(GATE_CONFIG) as fh:
        return json.load(fh)


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


def summarize(run):
    gate = load_gate()
    files = {}
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
            # Page-1 text agreement with pdftotext (report only; absent:
            # not compared, or an older runner).
            "text_f1": text_value(f, "text_char_f1"),
            "text_f1_annots": text_value(f, "text_char_f1_with_annots"),
        }
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
    return {"aggregate": agg, "files": files}


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
    graph comparison equal or exactly the declared change."""
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
            "version_agrees", "version_differs",
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
        if full and sem in ("pass", "transformed", "not_applicable", None) and (graph or rt.get("structure_declared")):
            fig["full_pass_verified"] += 1
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


def check(run, base, summary_md=None):
    gate = load_gate()
    cur = summarize(run)["files"]
    old = base["files"]
    armed = any("quality" in o for o in old.values())
    regressions, improvements, known_flags, unrecorded = [], [], [], []
    new_flags = []
    new_reports, known_reports = [], []
    semantic_changes = []
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
            if now is not None and semantic_moved(was, now):
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
        # Text agreement: reported, not enforced. Older baselines have no
        # values (nothing to compare).
        for key in ("text_f1", "text_f1_annots"):
            was, now = o.get(key), n.get(key)
            if was is not None and (now is None or now <= was - TEXT_F1_DROP):
                text_changes.append(f"{fid}: {key} {was} -> {now}")
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
    print(f"files: {len(cur)} now, {len(old)} in baseline, {len(missing)} missing, {len(added)} new")
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
    for line in regressions:
        print("REGRESSION:", line)
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


def main(argv):
    if len(argv) >= 3 and argv[1] == "summarize":
        with open(argv[2]) as fh:
            json.dump(summarize(json.load(fh)), sys.stdout, indent=1, sort_keys=True)
        sys.stdout.write("\n")
        return 0
    if len(argv) >= 4 and argv[1] == "check":
        with open(argv[2]) as fh:
            run = json.load(fh)
        with open(argv[3]) as fh:
            base = json.load(fh)
        summary_md = None
        if len(argv) >= 6 and argv[4] == "--summary-md":
            summary_md = argv[5]
        return check(run, base, summary_md)
    if len(argv) >= 3 and argv[1] == "quality":
        with open(argv[2]) as fh:
            run = json.load(fh)
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
    sys.stderr.write(__doc__)
    return 2


if __name__ == "__main__":
    sys.exit(main(sys.argv))
