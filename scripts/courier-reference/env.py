#!/usr/bin/env python3
"""The reference environment's identity, for the Courier evaluation
(docs/courier-reference.md): OS, the renderers' and font stack's package
versions, and every font file fontconfig lists, with its SHA-256.

    scripts/courier-reference/env.py --json OUT [--e1 DIR]

With --e1, also E1's configuration and its four fonts (hashes), and the
font list under E1. Nothing here depends on a document.
"""
import argparse, hashlib, json, os, platform, subprocess


def out(argv, env=None):
    r = subprocess.run(argv, capture_output=True, text=True, env=env)
    return (r.stdout + r.stderr).strip()


def sha(p):
    try:
        return hashlib.sha256(open(p, "rb").read()).hexdigest()
    except OSError:
        return None


def fonts(env=None):
    files = sorted(set(out(["fc-list", "--format", "%{file}\\n"], env).splitlines()))
    return {f: sha(f) for f in files}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--json", required=True)
    ap.add_argument("--e1")
    a = ap.parse_args()
    env = {k: v for k, v in os.environ.items() if k not in ("FONTCONFIG_FILE", "FONTCONFIG_PATH")}
    osr = dict(l.split("=", 1) for l in open("/etc/os-release").read().splitlines() if "=" in l)
    pkgs = out(["dpkg-query", "-W", "-f", "${Package} ${Version}\\n", "fontconfig", "fontconfig-config", "libfontconfig1",
                "libfreetype6", "poppler-utils", "libpoppler*", "poppler-data", "mupdf-tools", "qpdf", "fonts-*"], env)
    doc = {
        "os": osr.get("PRETTY_NAME", "").strip('"'), "machine": platform.machine(),
        "pdftoppm": out(["pdftoppm", "-v"], env).splitlines()[0], "mutool": out(["mutool", "-v"], env).splitlines()[0],
        "packages": sorted(l for l in pkgs.splitlines() if l and "no packages found" not in l),
        "fonts": fonts(env),
        "fc_match": {f: out(["fc-match", f], env) for f in ("Courier", "Courier:bold", "Courier:italic",
                                                             "Courier New", "Helvetica", "Times", "monospace")},
    }
    if a.e1:
        conf = os.path.join(os.path.abspath(a.e1), "fonts.conf")
        e1env = dict(env, FONTCONFIG_FILE=conf)
        doc["e1"] = {"fonts_conf_sha256": sha(conf),
                     "fonts": {f: sha(os.path.join(a.e1, "fonts", f)) for f in sorted(os.listdir(os.path.join(a.e1, "fonts")))},
                     "font_list_added": sorted(set(fonts(e1env)) - set(doc["fonts"]))}
    json.dump(doc, open(a.json, "w"), indent=1)
    print(f"{doc['os']} {doc['machine']}; {doc['pdftoppm']}; {doc['mutool']}; {len(doc['fonts'])} font files")


if __name__ == "__main__":
    main()
