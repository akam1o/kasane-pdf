#!/usr/bin/env python3
"""Per-file diagnosis of the render blockers in a compat-runner report.

Reads a runner JSON report (compat/reports/run-*.json, gitignored) and
writes compat/reports/diag-render-blockers-<run>.json with, for every
expect:pass file whose strict render did not pass: the file id and
SHA-256, the first error, the generalised root cause, the standard's
position, whether strict mode should succeed, whether recovery mode
accepts it, whether an approximation is involved, the recovery-mode
outcome recorded by the runner, and an effort estimate. The
classification rules are the pattern table below; everything per file
stays in the gitignored report — only category counts belong in
documentation.

    scripts/diag-render-blockers.py compat/reports/latest.json
"""
import json, os, re, sys, time

# (regex on the first error, root cause, spec position, strict?, recovery?, approximate?, effort)
RULES = [
    (r"encrypted", "encryption", "§7.6: security handler required", "no (feature)", "no", "no", "5–8 d (v0.3.0)"),
    (r"no MediaBox", "page without /MediaBox", "Table 30: required, inheritable", "no (no default in the standard)", "yes (US Letter, RecoveryRule::MediaBoxDefault)", "no", "done"),
    (r"no startxref|xref offset .* beyond|expected 'xref'|unbalanced '\)'|expected word token|ASCII85Decode: byte", "damaged file structure", "§7.5: cross-reference table / syntax required", "no", "xref rebuild (recovery parse), then per-content outcome", "no", "varies"),
    (r"/Type /Template in page tree|cycle detected in page tree", "invalid page tree", "§7.7.3", "no", "no", "no", "—"),
    (r"non-embedded CIDFont", "Identity-ordering CID font without program", "§9.7.4: CIDs are glyph indices of an unknown font", "no", "no (unsound without /ToUnicode)", "would be (fallback face)", "2–3 d, only with a complete /ToUnicode"),
    (r"states without /AS", "/AP /N state dictionary without /AS and more than one state", "§12.5.5: /AS required", "no (no basis to choose)", "no (no basis to choose)", "no", "—"),
    (r"max_image_dimension", "image larger than the dimension limit", "Limits policy; DCT scaled decode would keep the limit", "yes after DCT downscale", "n/a", "no (exact at output size)", "2 d"),
    (r"knockout", "knockout transparency group", "§11.4.8", "yes (implemented)", "n/a", "no", "done"),
    (r"transfer/black-generation", "transfer function (/TR) other than /Default", "§10.4", "yes (implement TR functions)", "n/a", "no", "2–3 d"),
    (r"shading /Function yields", "shading function output count ≠ colour-space components", "§8.7.4.5: n outputs required", "no", "possible (replicate), not exact", "yes", "1 d"),
    (r"JBIG2Decode outside an image", "image-only filter on a non-image stream", "§7.4.7: JBIG2 is for image XObjects only", "no", "no", "no", "—"),
    (r"cannot allocate 0x0", "degenerate page box", "Table 30: positive extent expected", "no", "possible (Letter), RecoveryRule candidate", "no", "0.5 d"),
    (r"'Q' without matching", "stray Q", "§8.4.2: unbalanced", "no", "yes (RecoveryRule::IgnoredRestoreWithoutSave)", "no", "done"),
    (r"inline image data does not end with EI", "inline image without EI / wrong length", "§8.9.7", "no", "no (ambiguous data end)", "no", "—"),
    (r"not allowed inside a path object", "operator inside a path object", "§8.2 Figure 9", "no", "yes (RecoveryRule::OperatorInsidePathObject)", "no", "done"),
    (r"non-embedded (Symbol|ZapfDingbats)", "non-embedded Symbol / ZapfDingbats", "Standard 14 without program", "yes (symbol fallback, approximate)", "n/a", "yes (SubstKind::SymbolFallback)", "done"),
    (r"tiling pattern cell painting with its own pattern", "pattern cell cycle", "§8.7.3: recursion undefined", "no", "no", "no", "—"),
    (r"/Differences entries must be", "malformed /Differences", "§9.6.6.1", "no", "possible (skip bad entries)", "no", "0.5 d"),
    (r"form field value outside the Latin range", "non-Latin field value with a single-byte field font", "§12.7.3.3 appearance synthesis", "no (needs a Unicode field font)", "no", "would be", "2 d + asset"),
    (r"corrupt deflate stream", "corrupt Flate data", "§7.4.4", "no", "no", "no", "—"),
    (r"content stream ends inside a BT|unmatched 'q'", "open block at end of stream", "§7.8.2", "yes (closed with Warning::UnterminatedContentBlock)", "n/a", "no", "done"),
    (r"/DecodeParms must be", "malformed /DecodeParms", "§7.3.8.2", "no", "possible (ignore when it names nothing)", "no", "0.5 d"),
    (r"/Properties entry .* is undefined", "BDC property name not in resources", "§14.6.2", "no", "possible (treat as no OC)", "no", "0.5 d"),
    (r"no sound substitute for this font name", "symbolic non-embedded TrueType with unknown name", "§9.6.6.4: glyph set unknowable", "no", "no", "would be", "—"),
    (r"unknown operator", "unknown operator outside BX/EX", "§7.8.2", "no", "yes (RecoveryRule::IgnoredUnknownOperator)", "no", "done"),
    (r"/Annots entry is not an annotation dictionary", "non-dictionary /Annots entry", "Table 164", "no", "yes (RecoveryRule::IgnoredInvalidAnnotation)", "no", "done"),
    (r"appearance state is not a stream", "appearance state that is not a stream", "§12.5.5", "no", "yes (RecoveryRule::IgnoredInvalidAnnotation)", "no", "done"),
    (r"JBIG2 page is .* but the image dictionary says", "JBIG2 page size ≠ /Width /Height", "§7.4.7 / Table 89", "no", "possible (trust the codestream), approximate", "yes", "1 d"),
    (r"BrotliDecode", "/BrotliDecode (PDF 2.0 extension)", "ISO 32000-2 extension", "no (feature)", "no", "no", "1–2 d + dependency"),
    (r"/RI must be", "invalid rendering intent", "§8.6.5.8: fallback RelativeColorimetric", "yes (Warning::UnknownRenderingIntent)", "n/a", "no", "done"),
    (r"JPEG in .* decodes to 0 components", "undecodable DCT data", "§7.4.8", "no", "no", "no", "—"),
    (r"referenced object .* is not defined", "reference to an undefined object", "§7.3.10: null", "yes (Warning::UndefinedReference)", "n/a", "no", "done"),
    (r"is not a dictionary", "null resource (undefined reference) where a dictionary is required", "§7.3.10 + the resource rule", "no", "possible (default font), approximate", "would be", "1 d"),
]


def classify(detail):
    for pat, cause, spec, strict, recovery, approx, effort in RULES:
        if re.search(pat, detail):
            return dict(root_cause=cause, spec=spec, strict_should_pass=strict, recovery_accepts=recovery, approximate=approx, effort=effort)
    return dict(root_cause="unclassified", spec="?", strict_should_pass="?", recovery_accepts="?", approximate="?", effort="?")


def main():
    src = sys.argv[1] if len(sys.argv) > 1 else "compat/reports/latest.json"
    r = json.load(open(src))
    run = r.get("run_id") or os.path.basename(os.path.realpath(src)).replace(".json", "")
    out = []
    counts = {}
    for f in r["files"]:
        if f.get("expect") != "pass" or f["render"]["class"] == "pass":
            continue
        stage = "render" if f["parse"]["class"] == "pass" else "parse"
        detail = f[stage].get("detail") or ""
        c = classify(detail)
        rec = f.get("render_recovered") or {}
        out.append(dict(id=f["id"], sha256=f.get("sha256"), stage=stage, cls=f[stage]["class"], first_error=detail,
                        recovered_render=rec.get("class"), recovered_detail=rec.get("detail"),
                        render_approximate=f.get("render_approximate"), recovery_notes=f.get("recovery_notes"),
                        warnings=len((f.get("metrics") or {}).get("warnings") or []), causes=f.get("causes"), **c))
        key = (c["root_cause"], c["strict_should_pass"], c["recovery_accepts"])
        counts[key] = counts.get(key, 0) + 1
    dst = os.path.join(os.path.dirname(src) or ".", f"diag-render-blockers-{run}.json")
    json.dump({"generated": time.strftime("%Y-%m-%dT%H:%M:%S"), "source": os.path.basename(src), "files": out}, open(dst, "w"), indent=1)
    print(f"{len(out)} blocker(s) -> {dst} (gitignored)")
    print("category (root cause | strict? | recovery?) : files")
    for (cause, strict, rec), n in sorted(counts.items(), key=lambda kv: -kv[1]):
        print(f"  {n:3d}  {cause} | {strict} | {rec}")


if __name__ == "__main__":
    main()
