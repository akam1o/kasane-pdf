#!/usr/bin/env python3
"""Independent audit of `UsageRightsPolicy::Remove` outputs with qpdf and pypdf.

Usage:
    usage-rights-audit.py INPUT.pdf OUTPUT.pdf [INPUT.pdf OUTPUT.pdf ...] [--json OUT]
    usage-rights-audit.py --self-test

For each (input, output) pair — however the output was produced (e.g.
`cargo run --example remove_usage_rights -- in.pdf out.pdf --carry`) —
both files are read by two readers that share no code with kasane-pdf:

- qpdf: `--check` (exit code, warnings) and a `--json=2` dump of every
  physical object;
- pypdf (strict): the graph reachable from the trailer `/Root` and `/Info`.

Each searches for the signature markers of `docs/plans/rewrite-policy.md`
§5.3.2: `/ByteRange`; `/Type /Sig` or `/DocTimeStamp`; `/Contents` with
`/Filter` or `/SubFilter`; `/Cert` with `/Contents`; `/Reference` with
`/TransformMethod`. A pair passes when the input has a marker for both
readers (the detectors see the signature), and the output has none
(qpdf over every object, pypdf over the reachable graph), no `/ByteRange`,
no `/Perms /UR`, `/UR3` or `/DocMDP`, `qpdf --check` exit 0 or 3 with no
more warnings than the input, the same page count for both readers, and
`/SigFlags` equal to the input's with bit 1 cleared.

Output is content-free: counts, key names, exit codes, file basenames.
Signature values, certificates and strings are never printed. Exit 0 when
every pair passes, 1 otherwise, 2 on usage errors.

`--self-test` builds a synthetic usage-rights form, removes it with the
`remove_usage_rights` example (cargo), and audits the pair; it also checks
that a hand-made output with a `/ByteRange` left behind fails. Needs
qpdf on PATH and pypdf importable.
"""
import json
import os
import subprocess
import sys
import tempfile

def marked(keys, type_name, reference_has_transform):
    k = set(keys)
    if "ByteRange" in k:
        return True
    if "Contents" in k and ("Filter" in k or "SubFilter" in k):
        return True
    if "Cert" in k and "Contents" in k:
        return True
    if type_name in ("Sig", "DocTimeStamp"):
        return True
    return "Reference" in k and reference_has_transform()


# ------------------------------------------------------------------ qpdf
def qpdf_scan(path):
    chk = subprocess.run(["qpdf", "--check", path], capture_output=True, text=True)
    warnings = sum(1 for l in (chk.stdout + chk.stderr).splitlines() if l.startswith("WARNING"))
    js = subprocess.run(["qpdf", "--json=2", "--json-key=qpdf", path], capture_output=True, text=True)
    if js.returncode not in (0, 3):
        return {"check_exit": chk.returncode, "warnings": warnings, "json_error": js.returncode}
    objs = json.loads(js.stdout)["qpdf"][1]

    def body(ref):
        o = objs.get(f"obj:{ref}")
        return None if o is None else o.get("value", (o.get("stream") or {}).get("dict"))

    def deref(v, depth=0):
        while isinstance(v, str) and v.endswith(" R") and depth < 64:
            v, depth = body(v), depth + 1
        return v

    def has_tm(v, depth=0):
        v = deref(v)
        if depth > 8:
            return True
        if isinstance(v, list):
            return any(has_tm(x, depth + 1) for x in v)
        return isinstance(v, dict) and "/TransformMethod" in v

    markers = byte_ranges = 0
    for k, o in objs.items():
        if not k.startswith("obj:"):
            continue
        stack = [o.get("value", (o.get("stream") or {}).get("dict"))]
        while stack:
            x = stack.pop()
            if isinstance(x, list):
                stack.extend(x)
            elif isinstance(x, dict):
                t = deref(x.get("/Type"))
                tn = t[1:] if isinstance(t, str) and t.startswith("/") else None
                if marked([kk[1:] for kk in x if kk.startswith("/")], tn, lambda x=x: has_tm(x.get("/Reference"))):
                    markers += 1
                byte_ranges += "/ByteRange" in x
                stack.extend(x.values())
    cat = deref((objs.get("trailer") or {}).get("value", {}).get("/Root")) or {}
    perms = deref(cat.get("/Perms")) if isinstance(cat, dict) else None
    af = deref(cat.get("/AcroForm")) if isinstance(cat, dict) else None
    sf = deref(af.get("/SigFlags")) if isinstance(af, dict) else None
    pages = subprocess.run(["qpdf", "--show-npages", path], capture_output=True, text=True).stdout.strip()
    return {
        "check_exit": chk.returncode,
        "warnings": warnings,
        "objects": sum(1 for k in objs if k.startswith("obj:")),
        "markers": markers,
        "byte_ranges": byte_ranges,
        "perms_keys": sorted(perms) if isinstance(perms, dict) else None,
        "perms_malformed": perms is not None and not isinstance(perms, dict),
        "sig_flags": sf if isinstance(sf, int) else None,
        "pages": int(pages) if pages.isdigit() else None,
    }


# ----------------------------------------------------------------- pypdf
def pypdf_scan(path):
    import pypdf
    from pypdf.generic import ArrayObject, DictionaryObject, IndirectObject, NameObject

    r = pypdf.PdfReader(path, strict=True)

    def res(v, depth=0):
        while isinstance(v, IndirectObject) and depth < 64:
            v, depth = v.get_object(), depth + 1
        return v

    def has_tm(v, depth=0):
        v = res(v)
        if depth > 8:
            return True
        if isinstance(v, ArrayObject):
            return any(has_tm(x, depth + 1) for x in v)
        return isinstance(v, DictionaryObject) and "/TransformMethod" in v

    seen, markers, byte_ranges = set(), 0, 0
    stack = [r.trailer.get("/Root"), r.trailer.get("/Info")]
    while stack:
        x = stack.pop()
        if isinstance(x, IndirectObject):
            key = (x.idnum, x.generation)
            if key in seen:
                continue
            seen.add(key)
            x = x.get_object()
        if isinstance(x, ArrayObject):
            stack.extend(x)
        elif isinstance(x, DictionaryObject):  # streams are dictionaries too
            t = res(x.get("/Type"))
            tn = str(t)[1:] if isinstance(t, NameObject) else None
            if marked([str(k)[1:] for k in x.keys()], tn, lambda x=x: has_tm(x.get("/Reference"))):
                markers += 1
            byte_ranges += "/ByteRange" in x
            stack.extend(v for k, v in x.items() if k != "/Length")
    cat = r.trailer["/Root"].get_object()
    perms = res(cat.get("/Perms"))
    af = res(cat.get("/AcroForm"))
    sf = res(af.get("/SigFlags")) if isinstance(af, DictionaryObject) else None
    return {
        "reachable_objects": len(seen),
        "markers": markers,
        "byte_ranges": byte_ranges,
        "pages": len(r.pages),
        "perms_keys": sorted(str(k)[1:] for k in perms) if isinstance(perms, DictionaryObject) else None,
        "perms_malformed": perms is not None and not isinstance(perms, DictionaryObject),
        "sig_flags": int(sf) if sf is not None else None,
    }


def scan(path):
    out = {}
    for name, fn in (("qpdf", qpdf_scan), ("pypdf", pypdf_scan)):
        try:
            out[name] = fn(path)
        except Exception as e:  # recorded, never hidden; content-free
            out[name] = {"error": type(e).__name__}
    return out


def judge(i, o):
    """The reasons a pair fails (empty: it passes)."""
    why = []
    for tool in ("qpdf", "pypdf"):
        if "error" in i[tool] or "json_error" in i[tool]:
            why.append(f"{tool}: input unreadable")
        if "error" in o[tool] or "json_error" in o[tool]:
            why.append(f"{tool}: output unreadable")
    if why:
        return why
    for tool in ("qpdf", "pypdf"):
        if i[tool]["markers"] < 1:
            why.append(f"{tool}: no signature marker in the input")
        if o[tool]["markers"] or o[tool]["byte_ranges"]:
            why.append(f"{tool}: {o[tool]['markers']} marker(s), {o[tool]['byte_ranges']} /ByteRange in the output")
        if o[tool].get("perms_malformed"):
            why.append(f"{tool}: the output /Perms is not a dictionary")
        left = set(o[tool]["perms_keys"] or ()) & {"UR", "UR3", "DocMDP"}
        if left:
            why.append(f"{tool}: /Perms still has {sorted(left)}")
        if o[tool]["pages"] != i[tool]["pages"]:
            why.append(f"{tool}: page count {i[tool]['pages']} -> {o[tool]['pages']}")
        before = i[tool]["sig_flags"]
        want = None if before is None else before & ~1
        if o[tool]["sig_flags"] != want:
            why.append(f"{tool}: /SigFlags {before} -> {o[tool]['sig_flags']} (want {want})")
    if o["qpdf"]["check_exit"] not in (0, 3):
        why.append(f"qpdf --check exit {o['qpdf']['check_exit']}")
    if o["qpdf"]["warnings"] > i["qpdf"]["warnings"]:
        why.append(f"qpdf warnings {i['qpdf']['warnings']} -> {o['qpdf']['warnings']}")
    return why


def audit(pairs):
    rows = []
    for inp, out in pairs:
        i, o = scan(inp), scan(out)
        rows.append({"input": os.path.basename(inp), "output": os.path.basename(out),
                     "input_scan": i, "output_scan": o, "failures": judge(i, o)})
    return rows


# -------------------------------------------------------------- self-test
UR3 = (b"<< /Type /Sig /Filter /Adobe.PPKLite /SubFilter /adbe.pkcs7.detached /ByteRange [0 10 20 30] "
       b"/Contents <00112233> /Reference [<< /Type /SigRef /TransformMethod /UR3 /TransformParams "
       b"<< /Type /TransformParams /V /2.2 /Form [/FillIn] >> >>] >>")


def build_pdf(objs):
    b = bytearray(b"%PDF-1.7\n")
    offs = {}
    for n, body in objs:
        offs[n] = len(b)
        b += f"{n} 0 obj\n".encode() + body + b"\nendobj\n"
    size = max(offs) + 1
    x = len(b)
    b += f"xref\n0 {size}\n0000000000 65535 f \n".encode()
    for n in range(1, size):
        b += (f"{offs[n]:010} 00000 n \n" if n in offs else "0000000000 65535 f \n").encode()
    b += f"trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{x}\n%%EOF\n".encode()
    return bytes(b)


def self_test():
    root = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
    with tempfile.TemporaryDirectory() as d:
        inp, out, bad = (os.path.join(d, f) for f in ("in.pdf", "out.pdf", "bad.pdf"))
        objs = [
            (1, b"<< /Type /Catalog /Pages 2 0 R /Perms << /UR3 10 0 R >> /AcroForm << /Fields [] /SigFlags 3 >> >>"),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
            (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>"),
            (10, UR3),
        ]
        open(inp, "wb").write(build_pdf(objs))
        r = subprocess.run(["cargo", "run", "-q", "--example", "remove_usage_rights", "--", inp, out],
                           cwd=root, capture_output=True, text=True)
        if r.returncode != 0:
            print("self-test: the example failed:", r.stderr[-400:])
            return 1
        # A hand-made bad output: the catalog without /Perms, the signature
        # dictionary still reachable, /SigFlags unchanged.
        bad_objs = [(1, b"<< /Type /Catalog /Pages 2 0 R /X 10 0 R /AcroForm << /Fields [] /SigFlags 3 >> >>")] + objs[1:]
        open(bad, "wb").write(build_pdf(bad_objs))
        scalar = os.path.join(d, "scalar.pdf")
        scalar_objs = [(1, b"<< /Type /Catalog /Pages 2 0 R /Perms 5 /AcroForm << /Fields [] /SigFlags 2 >> >>")] + objs[1:3]
        open(scalar, "wb").write(build_pdf(scalar_objs))
        good, wrong, odd = audit([(inp, out), (inp, bad), (inp, scalar)])
        ok = (not good["failures"] and len(wrong["failures"]) >= 3
              and any("not a dictionary" in w for w in odd["failures"]))
        print("self-test:", "ok" if ok else "FAILED",
              {"good": good["failures"], "bad": wrong["failures"], "scalar /Perms": odd["failures"]})
        return 0 if ok else 1


def main(argv):
    if argv[1:] == ["--self-test"]:
        return self_test()
    args, out_json = list(argv[1:]), None
    if "--json" in args:
        k = args.index("--json")
        out_json = args[k + 1]
        del args[k:k + 2]
    if not args or len(args) % 2:
        print(__doc__.split("\n\n")[1], file=sys.stderr)
        return 2
    rows = audit(list(zip(args[::2], args[1::2])))
    if out_json:
        with open(out_json, "w") as f:
            json.dump(rows, f, indent=1)
    failed = [r for r in rows if r["failures"]]
    for r in failed:
        print("FAIL", r["output"], "; ".join(r["failures"]))
    print(f"{len(rows) - len(failed)}/{len(rows)} pair(s) pass the qpdf and pypdf audit")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
