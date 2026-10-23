#!/usr/bin/env python3
"""Download / upload the real-world PDF compatibility corpus.

The corpus lives in an S3-compatible object store, keyed by content
SHA-256 so the manifest is immutable and cache-friendly. Nothing
PDF-shaped is ever committed to git. The store is intentionally
generic — Cloudflare R2, AWS S3, MinIO, Backblaze B2, etc. all work;
nothing here is tied to a specific provider or account.

Layout in the bucket:
    pdfs/<category>/<sha256>.pdf

Usage:
    fetch-corpus.py list                      # show manifest entries
    fetch-corpus.py download [--dir DIR]      # fetch all → DIR (default compat/corpus)
    fetch-corpus.py add <file> --category C --source S --license L [--notes N]
    fetch-corpus.py add-url <url>  ...        # download then add
    fetch-corpus.py verify                    # re-hash local files vs manifest
    fetch-corpus.py seed --dir DIR            # build a corpus dir from local PDFs (no store)
    fetch-corpus.py sync [--dir DIR]          # upload manifest entries the store lacks

`add`/`add-url --local` register a file in the manifest and keep it in
the corpus directory without touching the store (no credentials
needed); `sync` uploads those later. Every entry records `producer`
(normalised /Producer), `version` (%PDF header), `redistribute`
(whether the file itself may be published) and `expect`.

Configuration comes from the environment — no endpoint, bucket, or
credential is hard-coded. Copy `corpus.env.example` to `.env` (or set
the variables directly):

    KASANE_CORPUS_ENDPOINT   — S3 endpoint URL (e.g. an R2/S3/MinIO URL)
    KASANE_CORPUS_BUCKET     — bucket name
    KASANE_CORPUS_PREFIX     — object key prefix (default "pdfs")
    AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY  — credentials via the
                               standard boto3/AWS provider chain
                               (env, ~/.aws/credentials, IAM role, …).

`--dir DIR` with a locally-populated corpus skips the store entirely —
`download`/`verify`/`seed` all work against a plain directory.
"""

import argparse
import hashlib
import os
import sys
import re
import urllib.parse
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
# Object-key prefix inside the bucket. Overridable via env for anyone
# mirroring the corpus under a different layout.
KEY_PREFIX = os.environ.get("KASANE_CORPUS_PREFIX", "pdfs")


CATEGORIES = [
    "office", "browser", "latex", "papers", "japanese", "scan", "forms",
    "images", "graphics", "transparency", "annotations", "pdfa",
    "encrypted", "incremental", "malformed", "misc",
]


def _endpoint():
    """S3 endpoint URL — required for store access, never defaulted."""
    return (
        os.environ.get("KASANE_CORPUS_ENDPOINT")
        # Back-compat with the original variable names.
        or os.environ.get("R2_ENDPOINT")
        or os.environ.get("CLOUDFLARE_R2_ENDPOINT")
        or os.environ.get("AWS_ENDPOINT_URL")
        or os.environ.get("AWS_ENDPOINT_URL_S3")
    )


def _bucket():
    """Bucket name — required for store access, never defaulted."""
    return (
        os.environ.get("KASANE_CORPUS_BUCKET")
        or os.environ.get("R2_BUCKET")
        or os.environ.get("S3_BUCKET")
        or os.environ.get("AWS_S3_BUCKET")
    )


def client():
    ep = _endpoint()
    bk = _bucket()
    if not ep or not bk:
        sys.exit(
            "no corpus store configured: set KASANE_CORPUS_ENDPOINT and\n"
            "KASANE_CORPUS_BUCKET (see compat/corpus.env.example), or use\n"
            "a local directory with --dir."
        )
    # Credentials resolve through the standard boto3 provider chain —
    # env vars, ~/.aws/credentials + AWS_PROFILE, container/EC2 roles,
    # SSO — so no key material ever lives in this file or its config.
    return boto3.client(
        "s3",
        endpoint_url=ep,
        config=Config(
            signature_version="s3v4",
            region_name=os.environ.get("AWS_REGION", "auto"),
        ),
    )


def bucket():
    return _bucket()


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
    files = m.get("files", [])
    # If every file is already present+valid locally, no store needed.
    need = [
        e for e in files
        if not ((out / f"{e['id']}.pdf").exists()
                and sha256(out / f"{e['id']}.pdf") == e["sha256"])
    ]
    if not need:
        print(f"corpus already complete ({len(files)}) → {out}")
        return 0
    if not (_endpoint() and _bucket()):
        sys.exit(
            f"{len(need)} file(s) missing and no corpus store configured.\n"
            "Set KASANE_CORPUS_ENDPOINT/KASANE_CORPUS_BUCKET to fetch them,\n"
            "or point --dir at a locally-populated corpus."
        )
    s3 = client()
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


def cmd_seed(args):
    """Populate a corpus dir from local PDFs matching the manifest's
    sha256 — no object store required. Point `--from` at a directory of
    PDFs; each manifest entry is linked/copied in if its hash matches."""
    m = load_manifest()
    src = Path(args.source_dir)
    out = Path(args.dir)
    out.mkdir(parents=True, exist_ok=True)
    want = {e["sha256"]: e for e in m.get("files", [])}
    placed = 0
    for pdf in sorted(src.rglob("*.pdf")):
        digest = sha256(pdf)
        e = want.get(digest)
        if not e:
            continue
        dest = out / f"{e['id']}.pdf"
        if not dest.exists():
            dest.hardlink_to(pdf) if hasattr(Path, "hardlink_to") else \
                dest.write_bytes(pdf.read_bytes())
        placed += 1
    print(f"seeded {placed}/{len(want)} manifest entries → {out}")
    return 0


PDF_INFO_RE = {
    "producer": re.compile(rb"/Producer\s*(\((?:[^()\\]|\\.)*\)|<[0-9A-Fa-f\s]*>)"),
    "creator": re.compile(rb"/Creator\s*(\((?:[^()\\]|\\.)*\)|<[0-9A-Fa-f\s]*>)"),
}


def _pdf_string(raw):
    """Decode a literal or hex PDF string to text (best effort)."""
    if raw.startswith(b"<"):
        hx = re.sub(rb"\s", b"", raw[1:-1])
        try:
            b = bytes.fromhex(hx.decode("ascii"))
        except ValueError:
            return ""
    else:
        b = raw[1:-1].replace(b"\\(", b"(").replace(b"\\)", b")").replace(b"\\\\", b"\\")
    if b.startswith(b"\xfe\xff"):
        return b[2:].decode("utf-16-be", "replace")
    return b.decode("latin-1", "replace")


def normalise_producer(text):
    """Collapse a /Producer string to a product family — the axis the
    report groups by (version numbers and build ids dropped)."""
    t = text.strip().lower()
    if not t or not text.isprintable():
        return "unknown"
    rules = [
        ("pdftex", "pdfTeX"), ("xetex", "XeTeX"), ("luatex", "LuaTeX"),
        ("dvipdfm", "dvipdfmx"), ("dvips", "dvips"), ("ghostscript", "Ghostscript"),
        ("gpl ghostscript", "Ghostscript"), ("acrobat distiller", "Acrobat Distiller"),
        ("acrobat pdfmaker", "Acrobat PDFMaker"), ("adobe pdf library", "Adobe PDF Library"),
        ("livecycle", "LiveCycle Designer"), ("designer", "LiveCycle Designer"),
        ("pdf-xchange", "PDF-XChange"), ("pypdf", "pypdf"), ("pikepdf", "pikepdf"),
        ("openpdf", "OpenPDF"), ("jasperreports", "JasperReports"), ("crystal", "Crystal Reports"),
        ("pdfcreator", "PDFCreator"), ("doro", "Doro"), ("bullzip", "Bullzip"), ("cutepdf", "CutePDF"),
        ("primopdf", "PrimoPDF"), ("novapdf", "novaPDF"), ("win2pdf", "Win2PDF"),
        ("acrobat", "Acrobat"), ("adobe indesign", "InDesign"), ("indesign", "InDesign"),
        ("illustrator", "Illustrator"), ("photoshop", "Photoshop"),
        ("microsoft", "Microsoft Office"), ("word", "Microsoft Office"),
        ("excel", "Microsoft Office"), ("powerpoint", "Microsoft Office"),
        ("libreoffice", "LibreOffice"), ("openoffice", "OpenOffice"),
        ("skia/pdf", "Skia (Chrome)"), ("chrome", "Skia (Chrome)"),
        ("cairo", "cairo (Firefox)"), ("quartz", "Quartz (macOS)"),
        ("mac os x", "Quartz (macOS)"), ("macos", "Quartz (macOS)"),
        ("ios", "Quartz (iOS)"), ("prince", "Prince"), ("wkhtmltopdf", "wkhtmltopdf"),
        ("qt ", "Qt"), ("itext", "iText"), ("pdfbox", "PDFBox"), ("reportlab", "ReportLab"),
        ("pdfkit", "PDFKit"), ("fpdf", "FPDF"), ("tcpdf", "TCPDF"), ("dompdf", "dompdf"),
        ("mpdf", "mPDF"), ("pdf.js", "pdf.js"), ("pdfium", "PDFium"), ("qpdf", "qpdf"),
        ("pdftk", "pdftk"), ("mupdf", "MuPDF"), ("poppler", "poppler"),
        ("scribus", "Scribus"), ("inkscape", "Inkscape"), ("ilovepdf", "iLovePDF"),
        ("google", "Google Docs"), ("pages", "Apple Pages"), ("keynote", "Apple Keynote"),
        ("ichitaro", "Ichitaro"), ("一太郎", "Ichitaro"), ("justsystem", "Ichitaro"),
        ("antenna", "Antenna House"), ("fop", "Apache FOP"), ("pdflib", "PDFlib"),
        ("abbyy", "ABBYY FineReader"), ("finereader", "ABBYY FineReader"),
        ("canon", "Canon scanner"), ("ricoh", "Ricoh scanner"), ("xerox", "Xerox scanner"),
        ("kyocera", "Kyocera scanner"), ("epson", "Epson scanner"), ("hp ", "HP scanner"),
        ("brother", "Brother scanner"), ("fuji", "Fujifilm scanner"), ("konica", "Konica scanner"),
        ("scansnap", "ScanSnap"), ("naps2", "NAPS2"), ("tesseract", "Tesseract"),
        ("ocrmypdf", "OCRmyPDF"), ("image magick", "ImageMagick"), ("imagemagick", "ImageMagick"),
        ("latex", "LaTeX"), ("verapdf", "veraPDF"), ("pdf-xchange", "PDF-XChange"),
        ("foxit", "Foxit"), ("nitro", "Nitro"), ("pdfsharp", "PDFsharp"), ("docraptor", "DocRaptor"),
        ("weasyprint", "WeasyPrint"), ("typst", "Typst"), ("groff", "groff"),
        ("cups", "CUPS"), ("pstoedit", "pstoedit"),
    ]
    for needle, family in rules:
        if needle in t:
            return family
    # First word, title-cased, as a last resort.
    return re.split(r"[\s/;,(]", text.strip())[0][:40] or "unknown"


def _pdfinfo(path):
    """`{key: value}` from `pdfinfo` (sees /Info inside object streams,
    which the byte scan cannot); empty when pdfinfo is absent/fails."""
    import shutil
    import subprocess
    if not shutil.which("pdfinfo"):
        return {}
    try:
        out = subprocess.run(["pdfinfo", str(path)], capture_output=True,
                             text=True, errors="replace", timeout=30)
    except Exception:  # noqa: BLE001
        return {}
    info = {}
    for line in out.stdout.splitlines():
        k, _, v = line.partition(":")
        if _:
            info[k.strip()] = v.strip()
    return info


def pdf_facts(path):
    """`(producer family, raw producer, pdf version, encrypted, pages)`:
    the byte scan first (works on broken files), `pdfinfo` to fill
    what the scan cannot see (compressed /Info, page count)."""
    data = Path(path).read_bytes()
    m = re.search(rb"%PDF-(\d\.\d)", data[:1024])
    version = m.group(1).decode() if m else ""
    raw = ""
    m = PDF_INFO_RE["producer"].search(data)
    if m:
        raw = _pdf_string(m.group(1))
    if not raw:
        m = PDF_INFO_RE["creator"].search(data)
        if m:
            raw = _pdf_string(m.group(1))
    encrypted = b"/Encrypt" in data
    info = _pdfinfo(path)
    if not raw.strip() or not raw.isprintable():
        raw = info.get("Producer") or info.get("Creator") or ""
    if not version and info.get("PDF version"):
        version = info["PDF version"]
    pages = 0
    try:
        pages = int(info.get("Pages", "0"))
    except ValueError:
        pages = 0
    if not raw.isprintable():
        raw = ""
    return normalise_producer(raw), raw.strip()[:80], version, encrypted, pages


# Producer families that identify a document's *origin* better than a
# name-derived category guess — used by `refresh --recategorise`.
PRODUCER_CATEGORY = {
    "Microsoft Office": "office", "LibreOffice": "office", "OpenOffice": "office",
    "Google Docs": "office", "Apple Pages": "office", "Apple Keynote": "office",
    "Ichitaro": "office", "Acrobat PDFMaker": "office",
    "Skia (Chrome)": "browser", "cairo (Firefox)": "browser", "wkhtmltopdf": "browser",
    "Prince": "browser", "WeasyPrint": "browser", "dompdf": "browser",
    "pdfTeX": "latex", "XeTeX": "latex", "LuaTeX": "latex", "dvipdfmx": "latex",
    "dvips": "latex", "LaTeX": "latex", "Typst": "latex",
    "ABBYY FineReader": "scan", "Canon scanner": "scan", "Ricoh scanner": "scan",
    "Xerox scanner": "scan", "Kyocera scanner": "scan", "Epson scanner": "scan",
    "HP scanner": "scan", "Brother scanner": "scan", "Fujifilm scanner": "scan",
    "Konica scanner": "scan", "ScanSnap": "scan", "NAPS2": "scan", "Tesseract": "scan",
    "OCRmyPDF": "scan",
}


def cmd_refresh(args):
    """Recompute producer / version / encrypted / pages for every entry
    whose file is present locally (older entries predate those fields;
    compressed /Info needs pdfinfo). With `--recategorise`, a `misc`
    entry whose producer family names an origin (office suite,
    browser, TeX, scanner) moves to that category — recorded in notes."""
    m = load_manifest()
    out = Path(args.dir)
    changed = moved = 0
    for e in m.get("files", []):
        p = out / f"{e['id']}.pdf"
        if not p.exists():
            continue
        producer, raw, version, encrypted, pages = pdf_facts(p)
        new = {"producer": producer, "producer_raw": raw, "version": version,
               "encrypted": encrypted}
        if pages and not e.get("pages"):
            new["pages"] = pages
        e.setdefault("redistribute", "yes")
        e.setdefault("expect", "pass")
        if any(e.get(k) != v for k, v in new.items()):
            e.update(new)
            changed += 1
        if args.recategorise and e.get("category") == "misc":
            cat = PRODUCER_CATEGORY.get(producer)
            if cat:
                e["category"] = cat
                e["notes"] = (e.get("notes", "") + f" [category from producer {producer}]").strip()
                moved += 1
    save_manifest(m)
    print(f"refreshed {changed} entr{'y' if changed == 1 else 'ies'}, recategorised {moved}")
    return 0


def _register(path, category, source, license_, notes, pages, local=False,
              redistribute="yes", expect="pass", id_hint=None):
    m = load_manifest()
    digest = sha256(path)
    # Duplicate detection by content hash.
    for e in m.get("files", []):
        if e["sha256"] == digest:
            print(f"duplicate of {e['id']} ({digest[:12]}) — not added")
            return None
    eid = (id_hint or Path(path).stem).lower().replace(" ", "-")
    eid = re.sub(r"[^a-z0-9._-]+", "-", eid).strip("-") or digest[:12]
    ids = {e["id"] for e in m["files"]}
    base, i = eid, 2
    while eid in ids:
        eid = f"{base}-{i}"
        i += 1
    producer, raw_producer, version, encrypted, found_pages = pdf_facts(path)
    pages = pages or found_pages
    entry = {
        "id": eid,
        "sha256": digest,
        "category": category,
        "source": source,
        "license": license_,
        "expect": expect,
        "pages": pages,
        "notes": notes or "",
        "redistribute": redistribute,
        "producer": producer,
        "producer_raw": raw_producer,
        "version": version,
        "encrypted": encrypted,
    }
    if local:
        dest = DEFAULT_DIR / f"{eid}.pdf"
        DEFAULT_DIR.mkdir(parents=True, exist_ok=True)
        if not dest.exists() or sha256(dest) != digest:
            dest.write_bytes(Path(path).read_bytes())
        where = f"{dest} (local only — run `sync` to upload)"
    else:
        s3 = client()
        s3.upload_file(str(path), bucket(), key_for(entry),
                       ExtraArgs={"ContentType": "application/pdf"})
        where = f"s3://{bucket()}/{key_for(entry)}"
    m.setdefault("files", []).append(entry)
    save_manifest(m)
    print(f"added {eid} ({digest[:12]}, {producer}, PDF {version}) → {where}")
    return entry


def cmd_sync(args):
    """Upload every manifest entry whose object is missing from the
    store, taking the bytes from the corpus directory."""
    m = load_manifest()
    s3 = client()
    out = Path(args.dir)
    up = skip = miss = 0
    for e in m.get("files", []):
        src = out / f"{e['id']}.pdf"
        if not src.exists() or sha256(src) != e["sha256"]:
            print(f"  MISSING-LOCAL {e['id']}", file=sys.stderr)
            miss += 1
            continue
        key = key_for(e)
        try:
            s3.head_object(Bucket=bucket(), Key=key)
            skip += 1
            continue
        except Exception:  # noqa: BLE001 - absent → upload
            pass
        s3.upload_file(str(src), bucket(), key,
                       ExtraArgs={"ContentType": "application/pdf"})
        up += 1
    print(f"uploaded {up}, already present {skip}, missing locally {miss}")
    return 0 if miss == 0 else 1


def cmd_add(args):
    _register(args.file, args.category, args.source, args.license,
              args.notes, args.pages, local=args.local,
              redistribute=args.redistribute, expect=args.expect,
              id_hint=args.id)


def cmd_add_url(args):
    url = args.url
    tmp = Path("/tmp") / ("corpus-" + hashlib.sha256(url.encode()).hexdigest()[:16] + ".pdf")
    print(f"downloading {url}")
    req = urllib.request.Request(url, headers={"User-Agent": "kasane-corpus/1.0"})
    with urllib.request.urlopen(req, timeout=60) as r, open(tmp, "wb") as f:
        f.write(r.read())
    src = args.source or url
    hint = args.id or Path(urllib.parse.urlparse(url).path).stem
    _register(tmp, args.category, src, args.license, args.notes, args.pages,
              local=args.local, redistribute=args.redistribute,
              expect=args.expect, id_hint=hint)


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
        p.add_argument("--category", required=True, choices=CATEGORIES)
        p.add_argument("--source", required=(name == "add"))
        p.add_argument("--license", required=True)
        p.add_argument("--pages", type=int, default=0)
        p.add_argument("--notes", default="")
        p.add_argument("--local", action="store_true",
                       help="register + keep in the corpus dir; no upload")
        p.add_argument("--redistribute", choices=["yes", "no"], default="yes",
                       help="may the file itself be published with the corpus?")
        p.add_argument("--expect", choices=["pass", "reject", "any"], default="pass")
        p.add_argument("--id", default=None, help="manifest id (default: file stem)")

    vf = sub.add_parser("verify")
    vf.add_argument("--dir", default=str(DEFAULT_DIR))

    sy = sub.add_parser("sync", help="upload manifest entries missing from the store")
    sy.add_argument("--dir", default=str(DEFAULT_DIR))

    rf = sub.add_parser("refresh", help="recompute producer/version/pages for local files")
    rf.add_argument("--dir", default=str(DEFAULT_DIR))
    rf.add_argument("--recategorise", action="store_true",
                    help="move `misc` entries to the category their producer implies")

    sd = sub.add_parser("seed",
                        help="build a corpus dir from local PDFs (no store)")
    sd.add_argument("--from", dest="source_dir", required=True,
                    help="directory of PDFs to match against the manifest")
    sd.add_argument("--dir", default=str(DEFAULT_DIR))

    args = ap.parse_args()
    fn = {
        "list": cmd_list, "download": cmd_download, "add": cmd_add,
        "add-url": cmd_add_url, "verify": cmd_verify, "seed": cmd_seed,
        "sync": cmd_sync, "refresh": cmd_refresh,
    }[args.cmd]
    sys.exit(fn(args) or 0)


if __name__ == "__main__":
    main()
