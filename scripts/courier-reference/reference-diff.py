#!/usr/bin/env python3
"""Every page of a corpus rendered by Poppler in E0 and in E1: which pages
E1 changes, and which font files fontconfig gave Poppler for each file.

    scripts/courier-reference/reference-diff.py CORPUS_DIR E1DIR --json OUT [--dpi 96] [--jobs N]
        [--manifest compat/manifest.yaml] [--shard I/N] [--chunk J/M] [--progress FILE]

The runner's reference (`pdftoppm -r DPI -gray`), all pages, decoded
pixels compared by their SHA-256 (PGM headers and file metadata play no
part; no page is held in memory). Each Poppler process is bounded
(TIMEOUT seconds, MEMORY bytes of address space): a file neither
environment draws within them is not compared. A file Poppler cannot
draw whole (its page count from pdfinfo unknown or not reached, or a
failing status) is reported as partial or none — compared as far as both
environments drew it, never counted as fully compared. The JSON names
files by their corpus id with page counts and the basenames of the
system font files matched — no page content; it is for the private
evaluation record, its counts for the public report. A file whose pages
E1 changes must be one whose E0 trace matched a Liberation Mono or
DejaVu Sans Mono face that E1 replaced for Courier — anything else is
listed as unexplained.

**The corpus** is the manifest's (`--manifest`, default
compat/manifest.yaml): every id it lists, each file checked against its
SHA-256 before it is used. A file missing or not the manifest's is an
error of the unit, which is then not complete — never a smaller corpus.

**Units.** `--shard I/N` takes the files whose index in the sorted corpus
is I-1 modulo N, `--chunk J/M` the J-th of M consecutive parts of those:
the nightly runs each unit as its own step and uploads it, so a runner
lost part-way keeps the units already uploaded
(scripts/courier-reference/reference-diff-merge.py joins them). A unit's
JSON says what it is (`unit`), what it was given (the corpus file list's
digest, the files it took), where (both references' fingerprints,
scripts/compat_env.py; the commit, run and attempt) and whether it
finished (`complete`). `--progress` appends one line per file as it is
done (and fsyncs it): what was finished before a process or runner was
lost. Each file records its time, the peak RSS of each Poppler process
and the bytes it wrote — diagnostics, not measures.
"""
import argparse, concurrent.futures, glob, hashlib, json, os, re, shutil, signal, subprocess, sys, tempfile, time

COURIER_E0 = ("LiberationMono-", "DejaVuSansMono")
HERE = os.path.dirname(os.path.abspath(__file__))


def matched(trace):
    files, best = [], False
    for line in trace.splitlines():
        if line.startswith(("Best score", "First font")):
            best = True
        elif best:
            m = re.search(r'file: "([^"]+)"', line)
            if m:
                files.append(os.path.basename(m.group(1)))
                best = False
    return files


TIMEOUT = 120
MEMORY = 4 << 30  # per Poppler process: a decompression bomb must not take the machine


def _memory_limit_applies():
    """Whether this platform takes an address-space limit (Linux does;
    macOS refuses one): recorded with the limits, never assumed."""
    import resource
    try:
        soft, hard = resource.getrlimit(resource.RLIMIT_AS)
        resource.setrlimit(resource.RLIMIT_AS, (soft, hard))
        return sys.platform.startswith("linux")
    except (ValueError, OSError):
        return False


MEMORY_LIMITED = _memory_limit_applies()


def _limit():
    if MEMORY_LIMITED:
        import resource
        resource.setrlimit(resource.RLIMIT_AS, (MEMORY, MEMORY))


def _run(argv, env, log):
    """(status, peak RSS in KiB) of argv, or (None, rss) after TIMEOUT
    (killed). The child's own rusage (wait4), not the machine's."""
    import threading
    with open(log, "wb") as fh:
        p = subprocess.Popen(argv, stdout=fh, stderr=subprocess.STDOUT, env=env, preexec_fn=_limit)
    timed_out = threading.Event()

    def kill():
        timed_out.set()
        p.kill()
    timer = threading.Timer(TIMEOUT, kill)
    timer.start()
    try:
        _, status, ru = os.wait4(p.pid, 0)
    finally:
        timer.cancel()
    p.returncode = os.waitstatus_to_exitcode(status)
    rss = ru.ru_maxrss // (1024 if sys.platform == "darwin" else 1)
    return (None if timed_out.is_set() else p.returncode), rss


def pages(tool, pdf, out, dpi):
    """({page: (width, height, sha256 of pixels)}, matched fonts, status,
    diagnostics), or Nones after a timeout."""
    os.makedirs(out, exist_ok=True)
    env = {k: v for k, v in os.environ.items() if k not in ("FONTCONFIG_FILE", "FONTCONFIG_PATH")}
    log = os.path.join(out, "trace.log")
    status, rss = _run([tool, "-r", str(dpi), "-gray", pdf, os.path.join(out, "p")], dict(env, FC_DEBUG="1"), log)
    files = sorted(glob.glob(os.path.join(out, "p-*.pgm")))
    diag = {"rss_kb": rss, "out_bytes": sum(os.path.getsize(p) for p in files)}
    if status is None:
        return None, None, None, dict(diag, timeout=True)
    imgs = {}
    for p in files:
        with open(p, "rb") as fh:
            data = fh.read()
        parts = data.split(maxsplit=4)
        imgs[int(re.search(r"-(\d+)\.pgm$", p).group(1))] = (parts[1].decode(), parts[2].decode(),
                                                             hashlib.sha256(parts[4]).hexdigest())
        os.remove(p)
    with open(log, errors="replace") as fh:
        trace = fh.read()
    # A damaged file can fail after some pages: what was drawn is compared,
    # the status recorded.
    return imgs, matched(trace), status, diag


def page_count(pdf):
    """Poppler's page count (pdfinfo), or None when it cannot tell."""
    try:
        r = subprocess.run(["pdfinfo", pdf], capture_output=True, text=True, errors="replace", timeout=120)
    except subprocess.TimeoutExpired:
        return None
    m = re.search(r"^Pages:\s+(\d+)", r.stdout, re.M)
    return int(m.group(1)) if r.returncode == 0 and m else None


def one(args):
    pdf, e1, dpi = args
    fid = os.path.splitext(os.path.basename(pdf))[0]
    t0 = time.monotonic()
    expected = page_count(pdf)
    with tempfile.TemporaryDirectory() as tmp:
        i0, f0, rc0, d0 = pages("pdftoppm", pdf, os.path.join(tmp, "e0"), dpi)
        i1, f1, rc1, d1 = pages(os.path.join(e1, "bin", "pdftoppm"), pdf, os.path.join(tmp, "e1"), dpi)
    diag = {"seconds": round(time.monotonic() - t0, 2), "e0": d0, "e1": d1}
    if i0 is None and i1 is None:
        # Neither environment drew it in time: not compared (a coverage
        # gap, as a failing status is).
        return fid, {"pages": 0, "pages_e1": 0, "expected_pages": expected, "status": "timeout",
                     "coverage": "none", "pages_changed": 0, "changed_pages": [], "e0_fonts": [], "e1_fonts": [],
                     "diagnostics": diag}
    if i0 is None or i1 is None:
        return fid, {"error": "timeout_in_one", "diagnostics": diag}
    if rc0 != rc1:
        return fid, {"error": "status_differs", "status": [rc0, rc1], "diagnostics": diag}
    diff = sorted(p for p in set(i0) | set(i1) if i0.get(p) != i1.get(p))
    # Coverage: every page Poppler says the file has, drawn without error
    # in both; else partial (some pages, or a failing status) or none.
    coverage = ("complete" if rc0 == 0 and expected is not None and len(i0) == len(i1) == expected
                else "none" if not i0 and not i1 else "partial")
    return fid, {"pages": len(i0), "pages_e1": len(i1), "expected_pages": expected, "status": rc0,
                 "coverage": coverage, "pages_changed": len(diff), "changed_pages": diff,
                 "e0_fonts": sorted(set(f0)), "e1_fonts": sorted(set(f1)), "diagnostics": diag}


def summarize(res):
    """The summary of per-file results (one unit's or the merged ones)."""
    changed = {f: r for f, r in res.items() if r.get("pages_changed")}
    unexplained = sorted(f for f, r in changed.items()
                         if not any(x.startswith(COURIER_E0) for x in set(r["e0_fonts"]) - set(r["e1_fonts"]))
                         or any(not x.startswith(COURIER_E0) for x in set(r["e0_fonts"]) - set(r["e1_fonts"]))
                         or not any(x.startswith("NimbusMonoPS-") for x in r["e1_fonts"])
                         or set(r["e1_fonts"]) - set(r["e0_fonts"]) - {x for x in r["e1_fonts"] if x.startswith("NimbusMonoPS-")})
    nimbus_unchanged = sorted(f for f, r in res.items() if "error" not in r and not r["pages_changed"]
                              and any(x.startswith("NimbusMonoPS-") for x in r["e1_fonts"]))
    cov = {c: sorted(f for f, r in res.items() if r.get("coverage") == c) for c in ("complete", "partial", "none")}
    return {"files": len(res), "errors": sorted(f for f, r in res.items() if "error" in r),
            "pages_compared": sum(r.get("pages", 0) for r in res.values()),
            "coverage_complete": cov["complete"], "coverage_partial": cov["partial"], "coverage_none": cov["none"],
            "poppler_status_nonzero": sorted(f"{f}:{r['status']}" for f, r in res.items() if r.get("status")),
            "limits": {"timeout_s": TIMEOUT, "memory_bytes": MEMORY if MEMORY_LIMITED else None},
            "expected_pages_known": sum(r["expected_pages"] for r in res.values() if r.get("expected_pages")),
            "files_changed": len(changed), "pages_changed": sum(r["pages_changed"] for r in changed.values()),
            "unexplained": unexplained, "nimbus_used_without_change": nimbus_unchanged,
            "files_using_nimbus_in_e1": sorted(f for f, r in res.items() if any(x.startswith("NimbusMonoPS-")
                                                                                  for x in r.get("e1_fonts", [])))}


def fraction(spec, name):
    m = re.fullmatch(r"(\d+)/(\d+)", spec or "1/1")
    if not m or not 1 <= int(m.group(1)) <= int(m.group(2)):
        raise SystemExit(f"reference-diff: --{name} must be I/N with 1 <= I <= N")
    return int(m.group(1)), int(m.group(2))


def select(pdfs, shard, chunk):
    """The files of unit (shard, chunk): every N-th of the sorted corpus,
    then the J-th of M consecutive parts."""
    (i, n), (j, m) = shard, chunk
    mine = [p for k, p in enumerate(pdfs) if k % n == i - 1]
    size = -(-len(mine) // m) if mine else 0
    return mine[(j - 1) * size: j * size]


def manifest_files(path):
    """[(id, sha256)] of the manifest, sorted as the corpus is (by file
    name, `.pdf` included)."""
    import yaml
    with open(path) as fh:
        m = yaml.safe_load(fh)
    rows = [(str(e["id"]), str(e["sha256"])) for e in m["files"]]
    if not rows or len({i for i, _ in rows}) != len(rows):
        raise SystemExit("reference-diff: the manifest lists no file, or one twice")
    return sorted(rows, key=lambda r: r[0] + ".pdf")


def corpus_digest(rows):
    """The corpus by content: the manifest's ids and SHA-256s."""
    h = hashlib.sha256()
    for i, sha in rows:
        h.update(f"{i}\0{sha}\n".encode())
    return h.hexdigest()


def verified(path, want):
    try:
        with open(path, "rb") as fh:
            return hashlib.sha256(fh.read()).hexdigest() == want
    except OSError:
        return False


def harness_digest():
    """The scripts that decide a unit's result, by content."""
    h = hashlib.sha256()
    for name in ("reference-diff.py", os.path.join("..", "compat_env.py")):
        with open(os.path.join(HERE, name), "rb") as fh:
            h.update(name.encode() + b"\0" + hashlib.sha256(fh.read()).digest())
    return h.hexdigest()


def fingerprints(e1):
    sys.path.insert(0, os.path.dirname(HERE))
    import compat_env
    return {"e0": compat_env.reference("pdftoppm"), "e1": compat_env.reference(os.path.join(e1, "bin", "pdftoppm")),
            "poppler": compat_env.tool_versions().get("poppler")}


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("corpus")
    ap.add_argument("e1")
    ap.add_argument("--json", required=True)
    ap.add_argument("--dpi", type=int, default=96)
    ap.add_argument("--jobs", type=int, default=2)
    ap.add_argument("--shard")
    ap.add_argument("--chunk")
    ap.add_argument("--progress")
    ap.add_argument("--manifest", default=os.path.join(os.path.dirname(os.path.dirname(HERE)), "compat", "manifest.yaml"))
    a = ap.parse_args()
    shard, chunk = fraction(a.shard, "shard"), fraction(a.chunk, "chunk")
    rows = manifest_files(a.manifest)
    want_sha = dict(rows)
    pdfs = [os.path.join(a.corpus, i + ".pdf") for i, _ in rows]
    e1 = os.path.abspath(a.e1)
    # E0 is the system's Poppler: not E1's wrapper through PATH.
    real = shutil.which("pdftoppm")
    if not real or os.path.realpath(real).startswith(e1 + os.sep) or os.environ.get("FONTCONFIG_FILE"):
        print("reference-diff: E0 would not be the system's Poppler (PATH or FONTCONFIG_FILE points at E1)")
        return 2
    mine = select(pdfs, shard, chunk)
    unit = {"shard": list(shard), "chunk": list(chunk), "files": [os.path.splitext(os.path.basename(p))[0] for p in mine]}
    head = {"unit": unit, "corpus": {"files": len(rows), "sha256": corpus_digest(rows)}, "dpi": a.dpi,
            "harness_sha256": harness_digest(), "references": fingerprints(e1),
            "run": {k: os.environ.get(v) for k, v in (("commit", "GITHUB_SHA"), ("run_id", "GITHUB_RUN_ID"),
                                                       ("attempt", "GITHUB_RUN_ATTEMPT"))}}
    prog = open(a.progress, "a") if a.progress else None

    def record(obj):
        if prog:
            prog.write(json.dumps(obj, sort_keys=True) + "\n")
            prog.flush()
            os.fsync(prog.fileno())
    record(dict(head, event="start", time=time.time()))
    res, t0 = {}, time.monotonic()
    # Every file of the unit is the manifest's, or the unit is not complete.
    bad = [p for p in mine if not verified(p, want_sha[os.path.splitext(os.path.basename(p))[0]])]
    for p in bad:
        fid = os.path.splitext(os.path.basename(p))[0]
        res[fid] = {"error": "not_the_manifest_file"}
        record({"event": "file", "id": fid, "time": time.time(), "error": "not_the_manifest_file"})
    mine = [p for p in mine if p not in bad]
    with concurrent.futures.ThreadPoolExecutor(a.jobs) as ex:
        for fid, r in ex.map(one, [(p, e1, a.dpi) for p in mine]):
            res[fid] = r
            record({"event": "file", "id": fid, "time": time.time(), "coverage": r.get("coverage"),
                    "error": r.get("error"), "diagnostics": r.get("diagnostics")})
    complete = sorted(res) == sorted(unit["files"]) and not bad
    summary = summarize(res)
    doc = dict(head, complete=complete, seconds=round(time.monotonic() - t0, 1), summary=summary, files=res)
    with open(a.json + ".tmp", "w") as fh:
        json.dump(doc, fh, indent=1)
    os.replace(a.json + ".tmp", a.json)  # never a half-written unit
    record({"event": "end", "time": time.time(), "complete": complete})
    print(json.dumps({k: (v if not isinstance(v, list) else len(v)) for k, v in summary.items()}))
    if not complete:
        return 2
    return 1 if summary["unexplained"] or summary["errors"] else 0


if __name__ == "__main__":
    sys.exit(main())
