#!/usr/bin/env python3
"""Feature presence in the `expect: pass` corpus files, crossed with the
analysis outcomes (docs/compatibility-report.md § Feature usage).

    scripts/compat-featscan.py --run RUN.json --diag DIAG.json --attribution ATTR.json \
        --out compat/reports/featscan-<run>.json --summary compat/analysis/features-<version>.json

Each file is expanded with `qpdf --qdf --object-streams=disable
--decode-level=generalized` and searched with the patterns below — a
presence test, approximate (a key in an unused object counts; an
unusual spelling does not). Per feature, the summary counts the files
that use it, how many rendered strictly, how many are strict blockers
(scripts/diag-render-blockers.py), how many are below the attribution
threshold (scripts/compat-attribute.py) and how many of those the
attribution puts on kasane's side. These are co-occurrences, not
causes. --out (per-file booleans) belongs in the gitignored
compat/reports/; --summary holds counts only.
"""
import argparse
import concurrent.futures
import json
import os
import re
import subprocess

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))

PATTERNS = {
    "tiling": rb"/PatternType\s*1\b", "shading_pattern": rb"/PatternType\s*2\b",
    "ASCIIHex": rb"/ASCIIHexDecode|/AHx\b", "ASCII85": rb"/ASCII85Decode|/A85\b", "LZW": rb"/LZWDecode|/LZW\b",
    "RunLength": rb"/RunLengthDecode|/RL\b", "filter_chain": rb"/Filter\s*\[\s*/\w+\s*/\w+",
    "luminosity": rb"/S\s*/Luminosity", "alpha_smask": rb"/S\s*/Alpha", "knockout": rb"/K\s+true",
    "isolated": rb"/I\s+true", "blend": rb"/BM\s*/(?!Normal|Compatible)\w+",
    "shading1": rb"/ShadingType\s*1\b", "shading2_3": rb"/ShadingType\s*[23]\b", "shading4_7": rb"/ShadingType\s*[4-7]\b",
    "cff_cid": rb"/CIDFontType0C", "opentype": rb"/Subtype\s*/OpenType", "type1c": rb"/Subtype\s*/Type1C",
    "type3": rb"/Subtype\s*/Type3", "tounicode": rb"/ToUnicode", "actualtext": rb"/ActualText",
    "cmap_nonidentity": rb"/Encoding\s*/(?!Identity)[A-Za-z0-9]+-[HV]\b",
    "inline_image": rb"\sBI\s", "image_mask": rb"/ImageMask\s+true", "decode_array": rb"/Decode\s*\[",
    "icc": rb"/ICCBased", "indexed": rb"/Indexed", "separation": rb"/Separation", "devicen": rb"/DeviceN",
    "lab": rb"/Lab\b", "smask_in_data": rb"/SMaskInData", "matte": rb"/Matte",
    "xfa": rb"/XFA", "need_appearances": rb"/NeedAppearances\s+true", "freetext": rb"/Subtype\s*/FreeText",
    "widget": rb"/Subtype\s*/Widget", "embedded_files": rb"/EmbeddedFiles", "oc": rb"/OCProperties",
    "userunit": rb"/UserUnit",
}


def corpus_path(fid):
    base = os.environ.get("KASANE_CORPUS_DIR") or os.path.join(ROOT, "compat", "corpus")
    for name in (fid + ".pdf", fid):
        if os.path.isfile(os.path.join(base, name)):
            return os.path.join(base, name)
    return None


def scan(fid):
    p = corpus_path(fid)
    if p is None:
        return fid, None
    q = subprocess.run(["qpdf", "--qdf", "--object-streams=disable", "--decode-level=generalized", p, "-"],
                       capture_output=True, timeout=300).stdout
    raw = open(p, "rb").read()
    t = {k: bool(re.search(v, q)) for k, v in PATTERNS.items()}
    # Structure is a property of the file as stored, not of the expansion.
    t["objstm"] = bool(re.search(rb"/Type\s*/ObjStm", raw))
    t["xrefstm"] = bool(re.search(rb"/Type\s*/XRef", raw))
    t["incremental"] = raw.count(b"%%EOF") > 1
    return fid, t


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    for k in ("run", "diag", "attribution", "out", "summary"):
        ap.add_argument(f"--{k}", required=True)
    a = ap.parse_args()
    run = {f["id"]: f for f in json.load(open(a.run))["files"]}
    blockers = {r["id"] for r in json.load(open(a.diag))["files"]}
    att = {r["id"]: r["class"] for r in json.load(open(a.attribution))["files"]}
    ids = [i for i, f in run.items() if f["expect"] == "pass"]
    with concurrent.futures.ThreadPoolExecutor(8) as ex:
        res = {fid: t for fid, t in ex.map(scan, ids) if t is not None}
    json.dump(res, open(a.out, "w"))
    summary = {}
    for k in sorted({k for t in res.values() for k in t}):
        fs = [i for i in res if res[i].get(k)]
        strict = [i for i in fs if not run[i].get("recovered") and run[i]["render"]["class"] == "pass"]
        summary[k] = {"use": len(fs), "strict_render": len(strict), "blockers": sum(i in blockers for i in fs),
                      "below_0_95": sum(i in att for i in strict),
                      "kasane_class": sum(att.get(i) in ("kasane", "annotation_layer") for i in strict)}
    with open(a.summary, "w") as fh:
        json.dump({"description": "Feature presence among the expect:pass files (regex over a qpdf --qdf expansion; "
                                  "approximate), with strict-render, blocker and low-SSIM co-occurrence counts. "
                                  "Co-occurrence is not causation: see docs/compatibility-report.md.",
                   "population": "expect:pass", "scanned": len(res), "features": summary}, fh, indent=1, sort_keys=True)
        fh.write("\n")
    print(f"compat-featscan: {len(res)} files scanned -> {a.summary}")


if __name__ == "__main__":
    main()
