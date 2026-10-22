#!/usr/bin/env python3
"""Download / upload the real-world PDF compatibility corpus (R2).

PDFs live in Cloudflare R2 (S3 API), keyed by content SHA-256 so the
manifest is immutable and cache-friendly. Nothing PDF-shaped is ever
committed to git.

Layout in the bucket:
    pdfs/<category>/<sha256>.pdf

Usage:
    fetch-corpus.py list                      # show manifest entries
    fetch-corpus.py download [--dir DIR]      # fetch all → DIR (default compat/corpus)
    fetch-corpus.py add <file> --category C --source S --license L [--notes N]
    fetch-corpus.py add-url <url>  ...        # download then add
    fetch-corpus.py verify                    # re-hash local files vs manifest

Credentials come from the environment (or ~/.cloudflare/r2-* files):
    R2_ACCOUNT_ENDPOINT / CLOUDFLARE_R2_ENDPOINT   — S3 endpoint URL
    AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY     — token
    R2_BUCKET                                       — default kasane-pdf-corpus
"""

import argparse
import hashlib
import os
import sys
import urllib.request
from pathlib import Path

try:
    import boto3
    from botocore.config import Config
    import yaml
except ImportError:  # pragma: no cover - dependency hint
    sys.exit("this tool needs the corpus venv: source .venv-corpus/bin/activate")

ROOT = Path(__file__).resolve().parent
MANIFEST = ROOT / "manifest.yaml"
DEFAULT_DIR = ROOT / "corpus"
KEY_PREFIX = "pdfs"
DEFAULT_BUCKET = "kasane-pdf-corpus"
DEFAULT_ENDPOINT = "https://9dffe78260d2c3d85a616c9bc35a8485.r2.cloudflarestorage.com"


def _cred(name, filename):
    v = os.environ.get(name)
    if v:
        return v.strip()
    p = Path.home() / ".cloudflare" / filename
    return p.read_text().strip() if p.exists() else None


def client():
    ep = os.environ.get("R2_ENDPOINT") or os.environ.get("CLOUDFLARE_R2_ENDPOINT") \
        or DEFAULT_ENDPOINT
    ak = _cred("AWS_ACCESS_KEY_ID", "r2-kasane-pdf-corpus.access_key_id")
    sk = _cred("AWS_SECRET_ACCESS_KEY", "r2-kasane-pdf-corpus.secret_access_key")
    if not ak or not sk:
        sys.exit("no R2 credentials: set AWS_ACCESS_KEY_ID/AWS_SECRET_ACCESS_KEY "
                 "or write ~/.cloudflare/r2-kasane-pdf-corpus.*")
    return boto3.client(
        "s3",
        endpoint_url=ep,
        aws_access_key_id=ak,
        aws_secret_access_key=sk,
        config=Config(signature_version="s3v4", region_name="auto"),
    )


def bucket():
    return os.environ.get("R2_BUCKET", DEFAULT_BUCKET)


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def load_manifest():
    if MANIFEST.exists():
        return yaml.safe_load(MANIFEST.read_text()) or {"version": 1, "files": []}
    return {"version": 1, "files": []}


def save_manifest(m):
    MANIFEST.write_text(
        "# kasane-pdf real-world compatibility corpus manifest.\n"
        "# See docs/compat-corpus.md. Generated — edit via fetch-corpus.py.\n\n"
        + yaml.safe_dump(m, sort_keys=False, allow_unicode=True)
    )


def key_for(entry):
    return f"{KEY_PREFIX}/{entry['category']}/{entry['sha256']}.pdf"


def cmd_list(_args):
    m = load_manifest()
    files = m.get("files", [])
    print(f"{len(files)} files in manifest")
    for e in files:
        print(f"  {e['id']:32} {e['category']:12} {e.get('pages','?'):>3}p  "
              f"{e['sha256'][:12]}  {e.get('source','')[:40]}")


def cmd_download(args):
    m = load_manifest()
    out = Path(args.dir)
    out.mkdir(parents=True, exist_ok=True)
    s3 = client()
    files = m.get("files", [])
    ok = miss = 0
    for e in files:
        dest = out / f"{e['id']}.pdf"
        if dest.exists() and sha256(dest) == e["sha256"]:
            ok += 1
            continue
        try:
            s3.download_file(bucket(), key_for(e), str(dest))
        except Exception as ex:  # noqa: BLE001 - report and continue
            print(f"  MISS {e['id']}: {ex}", file=sys.stderr)
            miss += 1
            continue
        if sha256(dest) != e["sha256"]:
            print(f"  HASH-MISMATCH {e['id']}", file=sys.stderr)
            dest.unlink()
            miss += 1
            continue
        ok += 1
    print(f"downloaded {ok}, missing {miss} → {out}")
    return 0 if miss == 0 else 1


def _register(path, category, source, license_, notes, pages):
    m = load_manifest()
    digest = sha256(path)
    eid = Path(path).stem.lower().replace(" ", "-")
    # avoid id collisions
    ids = {e["id"] for e in m["files"]}
    base, i = eid, 2
    while eid in ids:
        eid = f"{base}-{i}"
        i += 1
    entry = {
        "id": eid,
        "sha256": digest,
        "category": category,
        "source": source,
        "license": license_,
        "pages": pages,
        "notes": notes or "",
    }
    s3 = client()
    s3.upload_file(str(path), bucket(), key_for(entry),
                   ExtraArgs={"ContentType": "application/pdf"})
    m.setdefault("files", []).append(entry)
    save_manifest(m)
    print(f"added {eid} ({digest[:12]}) → s3://{bucket()}/{key_for(entry)}")


def cmd_add(args):
    _register(args.file, args.category, args.source, args.license,
              args.notes, args.pages)


def cmd_add_url(args):
    url = args.url
    tmp = Path("/tmp") / ("corpus-" + hashlib.sha256(url.encode()).hexdigest()[:16] + ".pdf")
    print(f"downloading {url}")
    req = urllib.request.Request(url, headers={"User-Agent": "kasane-corpus/1.0"})
    with urllib.request.urlopen(req, timeout=60) as r, open(tmp, "wb") as f:
        f.write(r.read())
    src = args.source or url
    _register(tmp, args.category, src, args.license, args.notes, args.pages)


def cmd_verify(args):
    out = Path(args.dir)
    m = load_manifest()
    bad = 0
    for e in m.get("files", []):
        p = out / f"{e['id']}.pdf"
        if not p.exists():
            print(f"  MISSING {e['id']}")
            bad += 1
        elif sha256(p) != e["sha256"]:
            print(f"  CORRUPT {e['id']}")
            bad += 1
    print(f"{bad} problem(s)" if bad else "all present & intact")
    return 1 if bad else 0


def main():
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("list")

    dl = sub.add_parser("download")
    dl.add_argument("--dir", default=str(DEFAULT_DIR))

    for name in ("add", "add-url"):
        p = sub.add_parser(name)
        if name == "add":
            p.add_argument("file")
        else:
            p.add_argument("url")
        p.add_argument("--category", required=True,
                       choices=["office", "browser", "latex", "japanese", "scan",
                                "images", "graphics", "annotations", "encrypted",
                                "incremental", "malformed", "misc"])
        p.add_argument("--source", required=(name == "add"))
        p.add_argument("--license", required=True)
        p.add_argument("--pages", type=int, default=0)
        p.add_argument("--notes", default="")

    vf = sub.add_parser("verify")
    vf.add_argument("--dir", default=str(DEFAULT_DIR))

    args = ap.parse_args()
    fn = {
        "list": cmd_list, "download": cmd_download, "add": cmd_add,
        "add-url": cmd_add_url, "verify": cmd_verify,
    }[args.cmd]
    sys.exit(fn(args) or 0)


if __name__ == "__main__":
    main()
