#!/usr/bin/env python3
"""Grow the compatibility corpus from public, licence-clean sources.

Every file goes through `fetch-corpus.py`'s `_register(..., local=True)`:
hashed, de-duplicated by SHA-256, described (producer family, PDF
version, encryption), appended to `manifest.yaml` and kept in the
corpus directory. Nothing is uploaded here — `fetch-corpus.py sync`
pushes the local files to the object store when credentials are
available — and nothing PDF-shaped is ever committed to git.

Sources (each capped by a `--max-*` flag):

  pdfjs     mozilla/pdf.js test/pdfs (Apache-2.0) — real-world bug
            reproducers from many producers; categorised by name
  pdfium    pdfium testing/resources (BSD-3-Clause) — synthetic
            feature tests, forms, JS, XFA
  verapdf   veraPDF-corpus PDF/A-1b test suite (GPLv3+MPL; kept
            internal, redistribute: no)
  irs       IRS fillable forms (US Government, public domain)
  arxiv     arXiv papers whose OAI metadata carries a CC BY licence
            (redistributable with attribution)
  generated Chrome headless print-to-PDF of local HTML and of
            Wikipedia pages (CC BY-SA 4.0, attribution in notes),
            CUPS/Quartz text-to-PDF, image-only PDFs via `sips`
            (scan-like), and deterministic mutations of those files
            for the malformed category (expect: reject)

Run with the corpus venv: `.venv-corpus/bin/python compat/collect-corpus.py`.
"""

import argparse
import base64
import hashlib
import io
import json
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import importlib.util  # noqa: E402

_spec = importlib.util.spec_from_file_location(
    "fetch_corpus", Path(__file__).resolve().parent / "fetch-corpus.py"
)
fetch_corpus = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(fetch_corpus)

ROOT = Path(__file__).resolve().parent
CORPUS = ROOT / "corpus"
UA = "kasane-corpus-collector/1.0 (+https://github.com/akam1o/kasane-pdf)"
MAX_BYTES = 8 * 1024 * 1024


def log(msg):
    print(msg, flush=True)


def http_get(url, timeout=60, headers=None):
    req = urllib.request.Request(url, headers={"User-Agent": UA, **(headers or {})})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return r.read()


def manifest_index():
    m = fetch_corpus.load_manifest()
    return {e["sha256"] for e in m["files"]}, {e["id"] for e in m["files"]}, m


def page_count(path):
    try:
        out = subprocess.run(["pdfinfo", str(path)], capture_output=True, text=True, timeout=20)
        for line in out.stdout.splitlines():
            if line.startswith("Pages:"):
                return int(line.split()[1])
    except Exception:  # noqa: BLE001
        pass
    return 0


def register_bytes(data, id_hint, category, source, license_, notes="",
                   redistribute="yes", expect="pass"):
    if not data.startswith(b"%PDF") and b"%PDF" not in data[:1024]:
        log(f"  skip {id_hint}: not a PDF")
        return None
    if len(data) > MAX_BYTES:
        log(f"  skip {id_hint}: {len(data)} bytes > cap")
        return None
    digest = hashlib.sha256(data).hexdigest()
    known, _ids, _m = manifest_index()
    if digest in known:
        log(f"  dup  {id_hint}")
        return None
    with tempfile.NamedTemporaryFile(suffix=".pdf", delete=False) as f:
        f.write(data)
        tmp = Path(f.name)
    try:
        pages = page_count(tmp)
        return fetch_corpus._register(
            tmp, category, source, license_, notes, pages, local=True,
            redistribute=redistribute, expect=expect, id_hint=id_hint,
        )
    finally:
        tmp.unlink(missing_ok=True)


# ---------------------------------------------------------------------
# pdf.js
# ---------------------------------------------------------------------

def categorise_name(name):
    n = name.lower()
    rules = [
        (("annot", "popup", "highlight", "ink", "stamp", "freetext", "squiggly"), "annotations"),
        (("form", "widget", "acroform", "xfa", "field", "checkbox", "radio", "button"), "forms"),
        (("cid", "japan", "jp", "vertical", "cjk", "sjis", "euc", "unijis", "90ms", "mincho", "gothic",
          "arial_unicode", "noto", "korean", "chinese", "big5", "gbk"), "japanese"),
        (("ccitt", "jbig2", "jpx", "jp2", "scan", "fax"), "scan"),
        (("shading", "pattern", "blend", "alpha", "transparen", "smask", "group", "knockout",
          "gradient", "softmask"), "transparency"),
        (("image", "jpeg", "jpg", "png", "mask", "dct", "bmp", "tiff", "icc", "colorspace",
          "cmyk", "indexed", "separation", "devicen", "lab"), "images"),
        (("font", "type1", "truetype", "cff", "glyph", "text", "encoding", "cmap", "unicode",
          "type3", "opentype", "standard"), "graphics"),
        (("xref", "broken", "corrupt", "bad", "fuzz", "invalid", "missing", "truncat", "malformed"),
         "malformed"),
        (("encrypt", "aes", "rc4", "password", "protect"), "encrypted"),
        (("incremental", "prev", "update"), "incremental"),
        (("pdfa", "pdf-a", "pdf_a", "verapdf"), "pdfa"),
    ]
    for needles, cat in rules:
        if any(x in n for x in needles):
            return cat
    return "misc"


def collect_pdfjs(limit, seed):
    log("== pdf.js test/pdfs")
    listing = json.loads(http_get(
        "https://api.github.com/repos/mozilla/pdf.js/contents/test/pdfs?per_page=1000",
        headers={"Accept": "application/vnd.github+json"},
    ))
    pdfs = [e for e in listing if e["name"].lower().endswith(".pdf") and e["type"] == "file"
            and 0 < e["size"] <= MAX_BYTES]
    _known, ids, _m = manifest_index()
    pdfs = [e for e in pdfs if Path(e["name"]).stem.lower().replace(" ", "-") not in ids]
    rnd = random.Random(seed)
    rnd.shuffle(pdfs)
    # Keep category balance: round-robin over the name-derived category.
    buckets = {}
    for e in pdfs:
        buckets.setdefault(categorise_name(e["name"]), []).append(e)
    order = []
    while len(order) < len(pdfs):
        for cat in sorted(buckets):
            if buckets[cat]:
                order.append(buckets[cat].pop())
    added = 0
    for e in order:
        if added >= limit:
            break
        url = f"https://raw.githubusercontent.com/mozilla/pdf.js/master/test/pdfs/{urllib.parse.quote(e['name'])}"
        try:
            data = http_get(url)
        except Exception as ex:  # noqa: BLE001
            log(f"  fail {e['name']}: {ex}")
            continue
        if register_bytes(
            data, Path(e["name"]).stem, categorise_name(e["name"]),
            f"https://github.com/mozilla/pdf.js/tree/master/test/pdfs/{e['name']}",
            "Apache-2.0",
        ):
            added += 1
    log(f"pdf.js: added {added}")


# ---------------------------------------------------------------------
# pdfium
# ---------------------------------------------------------------------

def gitiles_json(url):
    raw = http_get(url)
    return json.loads(raw[raw.index(b"{"):])


def collect_pdfium(limit, seed):
    log("== pdfium testing/resources")
    base = "https://pdfium.googlesource.com/pdfium/+/refs/heads/main/testing/resources/"
    listing = gitiles_json(base + "?format=JSON")
    names = [e["name"] for e in listing.get("entries", [])
             if e["type"] == "blob" and e["name"].lower().endswith(".pdf")]
    rnd = random.Random(seed)
    rnd.shuffle(names)
    added = 0
    for name in names:
        if added >= limit:
            break
        try:
            b64 = http_get(base + urllib.parse.quote(name) + "?format=TEXT")
            data = base64.b64decode(b64)
        except Exception as ex:  # noqa: BLE001
            log(f"  fail {name}: {ex}")
            continue
        cat = categorise_name(name)
        if cat == "misc" and ("js" in name.lower() or "bug_" in name.lower()):
            cat = "misc"
        if register_bytes(
            data, "pdfium-" + Path(name).stem, cat,
            f"https://pdfium.googlesource.com/pdfium/+/refs/heads/main/testing/resources/{name}",
            "BSD-3-Clause",
        ):
            added += 1
    log(f"pdfium: added {added}")


# ---------------------------------------------------------------------
# veraPDF
# ---------------------------------------------------------------------

def collect_verapdf(limit, seed):
    log("== veraPDF-corpus")
    tree = json.loads(http_get(
        "https://api.github.com/repos/veraPDF/veraPDF-corpus/git/trees/master?recursive=1",
        headers={"Accept": "application/vnd.github+json"},
    ))
    paths = [t["path"] for t in tree.get("tree", [])
             if t["type"] == "blob" and t["path"].lower().endswith(".pdf")
             and t["path"].startswith("PDF_A-1b/") and t.get("size", 0) <= MAX_BYTES]
    _known, ids, _m = manifest_index()
    rnd = random.Random(seed)
    rnd.shuffle(paths)
    added = 0
    for p in paths:
        if added >= limit:
            break
        stem = Path(p).stem
        hint = "verapdf_" + re.sub(r"[^A-Za-z0-9.-]+", "-", stem).lower()
        if hint in ids:
            continue
        url = "https://raw.githubusercontent.com/veraPDF/veraPDF-corpus/master/" + urllib.parse.quote(p)
        try:
            data = http_get(url)
        except Exception as ex:  # noqa: BLE001
            log(f"  fail {p}: {ex}")
            continue
        expect = "pass" if "-pass-" in stem else "any"
        if register_bytes(
            data, hint, "pdfa",
            f"https://github.com/veraPDF/veraPDF-corpus/tree/master/{p}",
            "GPLv3+MPL (internal)", redistribute="no", expect=expect,
            notes="PDF/A-1b conformance test; '-fail-' files are valid PDFs that violate a PDF/A rule",
        ):
            added += 1
    log(f"veraPDF: added {added}")


# ---------------------------------------------------------------------
# IRS forms (public domain)
# ---------------------------------------------------------------------

IRS_FORMS = [
    "f1040", "f1040s1", "f1040s2", "f1040s3", "f1040sa", "f1040sb", "f1040sc", "f1040sd",
    "f1040se", "f1040es", "f1040x", "fw4", "fw9", "fw2", "f941", "f941x", "f1065", "f1120",
    "f1120s", "fss4", "f4868", "f8822", "f2848", "f1096", "f990", "f990ez", "f1099msc",
    "f1099nec", "f8863", "f8949", "f1098", "f2441", "f8812", "f5695", "f8962", "f4562",
    "f8829", "f1040sr", "f1040nr", "f709",
]


def collect_irs(limit):
    log("== IRS forms")
    added = 0
    for name in IRS_FORMS:
        if added >= limit:
            break
        url = f"https://www.irs.gov/pub/irs-pdf/{name}.pdf"
        try:
            data = http_get(url)
        except Exception as ex:  # noqa: BLE001
            log(f"  fail {name}: {ex}")
            continue
        if register_bytes(data, "irs-" + name, "forms", url, "US Gov public domain",
                          notes="AcroForm with fillable fields"):
            added += 1
        time.sleep(0.5)
    log(f"IRS: added {added}")


# ---------------------------------------------------------------------
# arXiv (CC BY only)
# ---------------------------------------------------------------------

def collect_arxiv_api(limit, seed):
    """Fallback when OAI-PMH is rate-limited: the Atom search API lists
    recent ids; each abstract page is checked for a CC BY licence link
    before the PDF is fetched (two requests per paper, ≥ 3 s apart)."""
    log("== arXiv via search API + abs-page licence check")
    rnd = random.Random(seed)
    cats = ["cs.CL", "cs.LG", "math.CO", "physics.optics", "stat.ML", "q-bio.NC", "eess.SP", "econ.EM"]
    rnd.shuffle(cats)
    added = 0
    for cat in cats:
        if added >= limit:
            break
        url = (f"https://export.arxiv.org/api/query?search_query=cat:{cat}"
               "&sortBy=submittedDate&sortOrder=descending&max_results=40")
        try:
            atom = http_get(url, timeout=60).decode("utf-8", "replace")
        except Exception as ex:  # noqa: BLE001
            log(f"  search {cat} failed: {ex}")
            time.sleep(3)
            continue
        ids = re.findall(r"<id>https?://arxiv.org/abs/([^<]+?)(?:v\d+)?</id>", atom)
        rnd.shuffle(ids)
        for aid in ids[:12]:
            if added >= limit:
                break
            time.sleep(3)
            try:
                page = http_get(f"https://arxiv.org/abs/{aid}", timeout=60).decode("utf-8", "replace")
            except Exception as ex:  # noqa: BLE001
                log(f"  abs {aid} failed: {ex}")
                continue
            lic = re.search(r'href="(https?://creativecommons.org/licenses/by/[^"]+)"', page)
            if not lic:
                continue
            title = re.search(r'<meta name="citation_title" content="([^"]*)"', page)
            title = title.group(1) if title else ""
            time.sleep(3)
            try:
                data = http_get(f"https://arxiv.org/pdf/{aid}", timeout=120)
            except Exception as ex:  # noqa: BLE001
                log(f"  fail {aid}: {ex}")
                continue
            if register_bytes(
                data, "arxiv-" + aid.replace("/", "-"), "papers",
                f"https://arxiv.org/abs/{aid}", f"CC BY ({lic.group(1)})",
                notes=f"{title[:80]}; licence from the abstract page",
            ):
                added += 1
    log(f"arXiv (API): added {added}")
    return added


def collect_arxiv(limit, seed):
    log("== arXiv (CC BY licence only)")
    rnd = random.Random(seed)
    sets = ["cs", "math", "physics", "q-bio", "stat", "econ", "eess"]
    rnd.shuffle(sets)
    added = 0
    for aset in sets:
        if added >= limit:
            break
        url = ("https://export.arxiv.org/oai2?verb=ListRecords&metadataPrefix=arXiv"
               f"&set={aset}&from=2025-01-01&until=2025-01-20")
        xml = None
        for attempt in range(4):
            try:
                xml = http_get(url, timeout=90).decode("utf-8", "replace")
                break
            except urllib.error.HTTPError as ex:
                wait = int(ex.headers.get("Retry-After", "20") or 20) if ex.code == 429 else 10
                log(f"  listing {aset}: HTTP {ex.code}, retry in {wait}s")
                time.sleep(min(wait, 60))
            except Exception as ex:  # noqa: BLE001
                log(f"  listing {aset} failed: {ex}")
                break
        if xml is None:
            continue
        recs = re.findall(r"<arXiv[^>]*>(.*?)</arXiv>", xml, re.S)
        rnd.shuffle(recs)
        for rec in recs:
            if added >= limit:
                break
            lic = re.search(r"<license>([^<]+)</license>", rec)
            if not lic or "creativecommons.org/licenses/by/" not in lic.group(1):
                continue
            aid = re.search(r"<id>([^<]+)</id>", rec).group(1)
            title = re.search(r"<title>(.*?)</title>", rec, re.S)
            title = re.sub(r"\s+", " ", title.group(1)).strip() if title else ""
            authors = re.findall(r"<keyname>([^<]+)</keyname>", rec)[:3]
            try:
                data = http_get(f"https://arxiv.org/pdf/{aid}", timeout=120)
            except Exception as ex:  # noqa: BLE001
                log(f"  fail {aid}: {ex}")
                time.sleep(3)
                continue
            if register_bytes(
                data, "arxiv-" + aid.replace("/", "-"), "papers",
                f"https://arxiv.org/abs/{aid}", f"CC BY ({lic.group(1)})",
                notes=f"{title[:80]} — {', '.join(authors)}",
            ):
                added += 1
            time.sleep(3)  # arXiv asks for ≥ 3 s between requests
    log(f"arXiv: added {added}")
    if added < limit:
        collect_arxiv_api(limit - added, seed)


# ---------------------------------------------------------------------
# Generated: Chrome, CUPS/Quartz, sips, mutations
# ---------------------------------------------------------------------

CHROME = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"

HTML_DOCS = {
    "article-columns": """<html><head><meta charset=utf-8><style>
body{font-family:Georgia,serif;margin:2cm;columns:2;column-gap:1.5em}
h1{column-span:all;font-family:Helvetica,sans-serif;background:linear-gradient(90deg,#036,#6cf);color:#fff;padding:.4em}
table{border-collapse:collapse;width:100%}td,th{border:1px solid #888;padding:4px}
.rot{display:inline-block;transform:rotate(-12deg) scale(1.2);opacity:.7;background:#fc6}
blockquote{border-left:4px solid #c33;padding-left:1em;font-style:italic}
</style></head><body><h1>Rendering stress: columns, tables, transforms</h1>
@@FILL1@@<table><tr><th>Producer</th><th>Version</th><th>Share</th></tr><tr><td>Skia</td><td>1.7</td><td>42%</td></tr><tr><td>Quartz</td><td>1.3</td><td>18%</td></tr></table>
<p><span class=rot>rotated &amp; translucent</span> emoji: 🎉🚀📄 — ligatures: office, affix.</p>
<blockquote>Any sufficiently advanced bug is indistinguishable from a feature.</blockquote>
<svg width=300 height=120><defs><linearGradient id=g><stop offset=0 stop-color=red /><stop offset=1 stop-color=blue /></linearGradient></defs>
<rect x=10 y=10 width=200 height=80 fill=url(#g) rx=12 /><circle cx=250 cy=50 r=40 fill=green fill-opacity=.5 /><text x=20 y=110>svg text</text></svg>
@@FILL2@@</body></html>""",
    "japanese-vertical": """<html><head><meta charset=utf-8><style>
body{margin:2cm;font-family:"Hiragino Mincho ProN","Hiragino Sans",serif;font-size:14pt}
.v{writing-mode:vertical-rl;height:18cm;line-height:2}
h1{font-family:"Hiragino Sans",sans-serif}
rt{font-size:60%}
</style></head><body><h1>縦書きと横書きの混在テスト</h1>
<div class=v><p>吾輩は猫である。名前はまだ無い。どこで生れたかとんと見当がつかぬ。何でも薄暗いじめじめした所でニャーニャー泣いていた事だけは記憶している。</p>
<p>「<ruby>東京<rt>とうきょう</rt></ruby>」「２０２６年１０月２３日」。句読点、約物、半角カナ ｱｲｳｴｵ、全角英数ＡＢＣ１２３。</p></div>
<p>横書き：日本語の文章には漢字・ひらがな・カタカナが混ざります。約物の位置（「」『』【】）と禁則処理も確認対象です。</p>@@FILL@@</body></html>""",
    "form-controls": """<html><head><meta charset=utf-8><style>body{font-family:Helvetica;margin:2cm}label{display:block;margin:.5em 0}</style></head>
<body><h2>申込フォーム / Application form</h2>
<label>Name <input value="山田 太郎" size=30></label><label>Email <input type=email value="taro@example.jp"></label>
<label><input type=checkbox checked> I agree</label><label><input type=radio name=r checked> A <input type=radio name=r> B</label>
<label>Plan <select><option>Basic<option selected>Pro</select></label><label>Notes<br><textarea rows=4 cols=50>multi
line</textarea></label>
<progress value=70 max=100></progress> <meter value=.6></meter> <button>Submit</button>@@FILL@@</body></html>""",
    "code-listing": """<html><head><meta charset=utf-8><style>body{margin:1.5cm}pre{font-family:Menlo,monospace;font-size:9pt;background:#f4f4f4;padding:1em;white-space:pre-wrap}
.k{color:#a0a}.s{color:#080}.c{color:#888}</style></head><body><h3>src/render.rs (excerpt)</h3><pre>@@CODE@@</pre>@@FILL@@</body></html>""",
}

LOREM = ("Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor "
         "incididunt ut labore et dolore magna aliqua. Ut enim ad minim veniam, quis nostrud "
         "exercitation ullamco laboris nisi ut aliquip ex ea commodo consequat. ")


def chrome_print(html_path, out_pdf, extra=()):
    cmd = [CHROME, "--headless=new", "--disable-gpu", "--no-first-run",
           "--no-default-browser-check", "--no-pdf-header-footer",
           f"--print-to-pdf={out_pdf}", *extra, str(html_path)]
    subprocess.run(cmd, capture_output=True, timeout=120)
    return Path(out_pdf).exists() and Path(out_pdf).stat().st_size > 0


def collect_generated(limit_browser, limit_web, limit_office, limit_scan, limit_malformed, seed):
    rnd = random.Random(seed)
    tmp = Path(tempfile.mkdtemp(prefix="kasane-gen-"))
    produced = []

    # ---- Chrome: local HTML --------------------------------------------
    if os.path.exists(CHROME):
        log("== generated: Chrome print-to-PDF (local HTML)")
        code = (ROOT.parent / "src" / "render.rs").read_text(errors="replace")[:6000]
        code = code.replace("&", "&amp;").replace("<", "&lt;")
        n = 0
        for name, tpl in HTML_DOCS.items():
            for variant in range(3):
                if n >= limit_browser:
                    break
                filler = "".join(f"<p>{LOREM * (variant + 1)}</p>" for _ in range(6 * (variant + 1)))
                html = (tpl.replace("@@CODE@@", code)
                        .replace("@@FILL1@@", filler[: len(filler) // 2])
                        .replace("@@FILL2@@", filler[len(filler) // 2:])
                        .replace("@@FILL@@", filler))
                src = tmp / f"{name}-{variant}.html"
                src.write_text(html, encoding="utf-8")
                out = tmp / f"chrome-{name}-{variant}.pdf"
                extra = []
                if variant == 1:
                    extra = ["--window-size=794,1123"]
                if chrome_print(src, out, extra):
                    cat = "japanese" if "japanese" in name else ("forms" if "form" in name else "browser")
                    e = register_bytes(
                        out.read_bytes(), out.stem, cat,
                        "self-generated: Chrome headless --print-to-pdf of local HTML",
                        "CC0 (self-generated)",
                        notes=f"local HTML '{name}' variant {variant}",
                    )
                    if e:
                        produced.append(out)
                        n += 1
        # ---- Chrome: live pages (Wikipedia, CC BY-SA 4.0) ---------------
        log("== generated: Chrome print-to-PDF (Wikipedia pages)")
        pages = [
            ("https://en.wikipedia.org/wiki/PDF", "wiki-en-pdf", "browser"),
            ("https://en.wikipedia.org/wiki/Typography", "wiki-en-typography", "browser"),
            ("https://en.wikipedia.org/wiki/Color_space", "wiki-en-color-space", "browser"),
            ("https://en.wikipedia.org/wiki/Bessel_function", "wiki-en-bessel", "browser"),
            ("https://ja.wikipedia.org/wiki/Portable_Document_Format", "wiki-ja-pdf", "japanese"),
            ("https://ja.wikipedia.org/wiki/日本語", "wiki-ja-nihongo", "japanese"),
            ("https://ja.wikipedia.org/wiki/富士山", "wiki-ja-fujisan", "japanese"),
            ("https://ja.wikipedia.org/wiki/活字", "wiki-ja-katsuji", "japanese"),
            ("https://ja.wikipedia.org/wiki/東京都", "wiki-ja-tokyo", "japanese"),
            ("https://ja.wikipedia.org/wiki/縦書きと横書き", "wiki-ja-tategaki", "japanese"),
            ("https://de.wikipedia.org/wiki/Schriftart", "wiki-de-schriftart", "browser"),
            ("https://fr.wikipedia.org/wiki/Typographie", "wiki-fr-typographie", "browser"),
            ("https://ko.wikipedia.org/wiki/한글", "wiki-ko-hangul", "browser"),
            ("https://zh.wikipedia.org/wiki/漢字", "wiki-zh-hanzi", "browser"),
            ("https://ar.wikipedia.org/wiki/خط_عربي", "wiki-ar-khatt", "browser"),
        ]
        n = 0
        for url, hint, cat in pages:
            if n >= limit_web:
                break
            out = tmp / f"chrome-{hint}.pdf"
            if chrome_print(url, out, ["--virtual-time-budget=8000"]):
                e = register_bytes(
                    out.read_bytes(), out.stem, cat,
                    url, "CC BY-SA 4.0 (Wikipedia text; printed with Chrome)",
                    notes="Wikipedia article printed with Chrome headless; attribution: Wikipedia contributors",
                )
                if e:
                    produced.append(out)
                    n += 1
    else:
        log("Chrome not found — browser-print files skipped")

    # ---- CUPS / Quartz text → PDF ----------------------------------------
    if shutil.which("cupsfilter"):
        log("== generated: cupsfilter (Quartz) text-to-PDF")
        n = 0
        texts = {
            "plain-latin": LOREM * 400,
            "plain-japanese": ("吾輩は猫である。名前はまだ無い。\n" * 50 + "縦書き横書き、句読点、約物。\n" * 50) * 4,
            "plain-tabs": "\n".join("\t".join(f"c{r}-{c}" for c in range(8)) for r in range(400)),
            "plain-long": "\n".join(f"line {i:05d}: {LOREM[:60]}" for i in range(2500)),
        }
        for name, text in texts.items():
            if n >= limit_office:
                break
            src = tmp / f"{name}.txt"
            src.write_text(text, encoding="utf-8")
            out = tmp / f"cups-{name}.pdf"
            with open(out, "wb") as f:
                subprocess.run(["cupsfilter", "-i", "text/plain", "-m", "application/pdf", str(src)],
                               stdout=f, stderr=subprocess.DEVNULL, timeout=120)
            if out.stat().st_size > 0:
                e = register_bytes(out.read_bytes(), out.stem, "office",
                                   "self-generated: cupsfilter text/plain → application/pdf (CUPS/Quartz)",
                                   "CC0 (self-generated)")
                if e:
                    produced.append(out)
                    n += 1

    # ---- sips: image-only (scan-like) PDFs from rendered pages ------------
    if shutil.which("sips") and shutil.which("pdftoppm"):
        log("== generated: sips image-only PDFs (scan-like)")
        bases = sorted(CORPUS.glob("*.pdf"))
        rnd.shuffle(bases)
        n = 0
        for base in bases:
            if n >= limit_scan:
                break
            if base.stat().st_size > 2 * 1024 * 1024:
                continue
            prefix = tmp / f"scan-{base.stem}"
            r = subprocess.run(["pdftoppm", "-r", "150", "-f", "1", "-l", "1", "-gray", "-png",
                                str(base), str(prefix)], capture_output=True, timeout=60)
            pngs = sorted(tmp.glob(f"scan-{base.stem}*.png"))
            if r.returncode != 0 or not pngs:
                continue
            out = tmp / f"scan-{base.stem}.pdf"
            subprocess.run(["sips", "-s", "format", "pdf", str(pngs[0]), "--out", str(out)],
                           capture_output=True, timeout=60)
            if out.exists() and out.stat().st_size > 0:
                e = register_bytes(out.read_bytes(), out.stem, "scan",
                                   f"self-generated: page 1 of corpus file '{base.stem}' rasterised (150 dpi gray) and wrapped by sips/Quartz",
                                   "derived from the source file's licence (image only)",
                                   notes="image-only page, no text layer")
                if e:
                    produced.append(out)
                    n += 1

    # ---- deterministic mutations → malformed --------------------------
    log("== generated: mutations (malformed, expect: reject)")
    bases = [p for p in produced if p.stat().st_size < 1024 * 1024]
    bases += sorted((ROOT.parent / "tests" / "fixtures").glob("*.pdf"))[:10]
    n = 0
    mutators = ["truncate-half", "truncate-tail", "flip-bytes", "zero-xref", "bad-startxref",
                "bad-lengths", "drop-endobj", "swap-objnums", "giant-count", "stray-bytes"]
    for i, base in enumerate(bases):
        if n >= limit_malformed:
            break
        data = base.read_bytes()
        mut = mutators[i % len(mutators)]
        r = random.Random(seed + i)
        out = bytearray(data)
        if mut == "truncate-half":
            out = out[: len(out) // 2]
        elif mut == "truncate-tail":
            out = out[: max(16, len(out) - r.randint(8, 200))]
        elif mut == "flip-bytes":
            for _ in range(max(1, len(out) // 100)):
                j = r.randrange(len(out))
                out[j] = r.randrange(256)
        elif mut == "zero-xref":
            k = data.rfind(b"xref")
            if k > 0:
                out[k:k + 200] = b"\0" * min(200, len(out) - k)
        elif mut == "bad-startxref":
            k = data.rfind(b"startxref")
            if k > 0:
                out[k + 10:k + 14] = b"9999"
        elif mut == "bad-lengths":
            out = bytearray(re.sub(rb"/Length \d+", b"/Length 7", bytes(out)))
        elif mut == "drop-endobj":
            out = bytearray(bytes(out).replace(b"endobj", b"      "))
        elif mut == "swap-objnums":
            out = bytearray(re.sub(rb"\n(\d+) 0 obj", lambda m: b"\n" + str(int(m.group(1)) * 7 % 50 + 1).encode() + b" 0 obj", bytes(out)))
        elif mut == "giant-count":
            out = bytearray(re.sub(rb"/Count \d+", b"/Count 2147483647", bytes(out)))
        elif mut == "stray-bytes":
            j = r.randrange(len(out))
            out[j:j] = bytes(r.randrange(256) for _ in range(512))
        e = register_bytes(bytes(out), f"mut-{mut}-{base.stem}", "malformed",
                           f"self-generated: '{mut}' mutation of {base.stem}",
                           "CC0 (self-generated mutation)", expect="reject",
                           notes=f"deterministic mutation '{mut}' (seed {seed + i})")
        if e:
            n += 1
    shutil.rmtree(tmp, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--max-pdfjs", type=int, default=200)
    ap.add_argument("--max-pdfium", type=int, default=60)
    ap.add_argument("--max-verapdf", type=int, default=40)
    ap.add_argument("--max-irs", type=int, default=25)
    ap.add_argument("--max-arxiv", type=int, default=15)
    ap.add_argument("--max-browser", type=int, default=12)
    ap.add_argument("--max-web", type=int, default=15)
    ap.add_argument("--max-office", type=int, default=4)
    ap.add_argument("--max-scan", type=int, default=12)
    ap.add_argument("--max-malformed", type=int, default=30)
    ap.add_argument("--only", nargs="*", default=None,
                    choices=["pdfjs", "pdfium", "verapdf", "irs", "arxiv", "generated"])
    ap.add_argument("--seed", type=int, default=20261023)
    a = ap.parse_args()
    steps = a.only or ["pdfjs", "pdfium", "verapdf", "irs", "arxiv", "generated"]
    for step in steps:
        try:
            if step == "pdfjs":
                collect_pdfjs(a.max_pdfjs, a.seed)
            elif step == "pdfium":
                collect_pdfium(a.max_pdfium, a.seed)
            elif step == "verapdf":
                collect_verapdf(a.max_verapdf, a.seed)
            elif step == "irs":
                collect_irs(a.max_irs)
            elif step == "arxiv":
                collect_arxiv(a.max_arxiv, a.seed)
            elif step == "generated":
                collect_generated(a.max_browser, a.max_web, a.max_office, a.max_scan,
                                  a.max_malformed, a.seed)
        except Exception as ex:  # noqa: BLE001 — one source failing must not stop the rest
            log(f"!! {step} aborted: {ex!r}")
    _k, _i, m = manifest_index()
    log(f"manifest now has {len(m['files'])} entries")


if __name__ == "__main__":
    main()
