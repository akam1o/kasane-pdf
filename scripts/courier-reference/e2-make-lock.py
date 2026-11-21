#!/usr/bin/env python3
"""Write scripts/courier-reference/e2-inputs.json, the lock of the
isolated reference E2 (setup-e2.py), from an inventory of a reference
machine — run by a maintainer when the reference fonts are to change,
never by CI.

    scripts/courier-reference/e2-make-lock.py INVENTORY.json --snapshot URL --out e2-inputs.json \\
        --package NAME=VERSION[@ARCH] ...

INVENTORY.json: {"fonts": [{"real": path, "sha256": ...}], "conf":
[{"path": conf.d path, "real": target, "sha256": ...}]} — what fc-list
and fc-conflist list on that machine (the runner image, or a container
of its packages). Each package is downloaded from the snapshot
(`<URL>/pool/main/<x>/<source>/<name>_<version>_<arch>.deb`, its SHA-256
recorded), and every inventory file must be found in one of them with
the same SHA-256 — so the lock reproduces that machine's fonts and
configuration from the packages alone. 50-user.conf and 51-local.conf
(per-user and local configuration) are left out on purpose.
"""
import argparse, hashlib, io, json, os, sys, tempfile, urllib.request

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from e2lib import deb_members  # noqa: E402

EXCLUDED_CONF = ("50-user.conf", "51-local.conf")
UA = {"User-Agent": "kasane-pdf e2-make-lock"}


def fetch(url):
    with urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=300) as r:
        return r.read()


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("inventory")
    ap.add_argument("--snapshot", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--package", action="append", required=True, help="NAME=VERSION[@ARCH]")
    ap.add_argument("--source", action="append", default=[], help="NAME=SOURCE (pool directory, when not NAME)")
    a = ap.parse_args()
    inv = json.load(open(a.inventory))
    sources = dict(s.split("=", 1) for s in a.source)
    packages, contents = [], {}
    for spec in a.package:
        name, rest = spec.split("=", 1)
        version, _, arch = rest.partition("@")
        arch = arch or "all"
        src = sources.get(name, name)
        fname = f"{name}_{version.split(':')[-1]}_{arch}.deb"
        path = f"pool/main/{src[:4] if src.startswith('lib') else src[0]}/{src}/{fname}"
        data = fetch(f"{a.snapshot}/{path}")
        packages.append({"name": name, "version": version, "arch": arch, "path": path,
                         "sha256": hashlib.sha256(data).hexdigest(), "size": len(data)})
        for member, body in deb_members(data):
            contents.setdefault(hashlib.sha256(body).hexdigest(), []).append((name, member))
        print(f"{name} {version}: {len(data)} bytes", file=sys.stderr)
    fonts, conf, missing = [], [], []
    for f in inv["fonts"]:
        hit = contents.get(f["sha256"])
        if not hit:
            missing.append(f["real"])
            continue
        fonts.append({"package": hit[0][0], "member": hit[0][1], "sha256": f["sha256"]})
    for c in inv["conf"]:
        base = os.path.basename(c["path"])
        if base in EXCLUDED_CONF or c["path"].endswith("/fonts.conf"):
            continue
        hit = contents.get(c["sha256"])
        if not hit:
            missing.append(c["path"])
            continue
        conf.append({"name": base, "package": hit[0][0], "member": hit[0][1], "sha256": c["sha256"]})
    top = [c for c in inv["conf"] if c["path"].endswith("/fonts.conf")]
    hit = contents.get(top[0]["sha256"]) if top else None
    if not hit:
        missing.append("fonts.conf")
    if missing:
        raise SystemExit(f"e2-make-lock: not in the packages: {missing}")
    lock = {"schema_version": 1, "snapshot": a.snapshot, "packages": packages,
            "fonts_conf": {"package": hit[0][0], "member": hit[0][1], "sha256": top[0]["sha256"]},
            "conf_d": sorted(conf, key=lambda c: c["name"]), "excluded_conf_d": list(EXCLUDED_CONF),
            "fonts": sorted(fonts, key=lambda f: f["member"])}
    with open(a.out, "w") as fh:
        json.dump(lock, fh, indent=1)
        fh.write("\n")
    print(f"e2-make-lock: {len(packages)} packages, {len(fonts)} fonts, {len(conf)} conf.d files", file=sys.stderr)


if __name__ == "__main__":
    main()
