#!/usr/bin/env python3
"""Evaluate `kasane-pdf redact` on a small, representative slice of the
real-world corpus (compat/corpus, see docs/compat-corpus.md).

The corpus has no redaction ground truth, so rects are generated from
the page itself (page 1 only), three kinds per file:

- word:  the box of a word of four or more letters near the middle of
         the page's text (Poppler `pdftotext -bbox`), padded 0.5 pt;
- line:  the same word's line height across the full page width — it
         crosses table rules, backgrounds and neighbouring text;
- block: the middle third of the page (figures, images, forms).

Each run is classified:

- ok:          redact succeeded and the independent checks passed;
- refused:     redact refused with a typed error and wrote nothing —
               an unsupported feature, or another typed error (malformed
               input, a limit, a signature policy); the reason is
               tallied;
- unexpected:  any other error, or a success that fails a check.

Checks on a success (tools that share no code with kasane):
- `qpdf --check` passes;
- removal: no Poppler word lies wholly inside the rect, and no pixel
  strictly inside it (2 px margin, 100 dpi) is non-white *and*
  unchanged from the input render — content left in place shows up as
  unchanged ink. Zeroed image samples turn black, so a sample that was
  already black looks unchanged and is counted too: such a run is
  reported unexpected and must be checked by hand (the check is not
  relaxed, since black content left over a zeroed image would look the
  same);
- preservation: words of the input clear of the rect (by 1 pt) are
  still extracted (also counted by character, which a regrouping of
  kept glyphs into other words does not change), and pixels outside the rect (2 px margin) are
  unchanged — the count is reported, not failed (a glyph straddling
  the edge is removed whole; square and round caps are cut
  conservatively);
- characters (MuPDF `mutool draw -F stext`, a second text engine,
  when installed): no character is extracted inside the rect, and each
  character of the input clear of the rect is found again at the same
  origin (to 0.01 pt) — lost and moved characters are counted
  separately from words pdftotext merely regroups;
- images: when an image is reported edited, the page's images
  (`pdfimages -all`) must not all come out unchanged unless an original
  is reported still drawn elsewhere;
- residual: the number of `elsewhere` findings is recorded;
- time: each run's `redact` wall time is recorded.

MuPDF rounds simple fonts' widths to integers: a kept glyph after
removed ones "moves" by hundredths of a point in its reading only.

A run whose only problem is unchanged ink inside where images were
zeroed is still `unexpected`, marked `check_by_hand` (zeroed samples
that were already black look unchanged, grey under a soft mask or
translucent paint; content left over a zeroed image would look the
same). It is then checked
mechanically: with no image of the output drawn (each image XObject an
empty form), no unchanged ink may stay inside — then it is all image samples, which `redact` verifies
are zero inside the rect (`unchanged_ink_not_from_images`, 0; the run
stays `unexpected`).

Pages whose geometry differs from plain MediaBox space (a /Rotate, a
CropBox, a MediaBox not at the origin) are skipped: the generated rects
would not map 1:1. Usage:

    scripts/eval-redact-corpus.py <kasane-pdf> [--per-category N] [--json out.json]
        [--redact-arg ARG …]   (passed to `redact`, e.g. --remove-annotations)
"""

import collections
import html
import json
import os
import re
import subprocess
import sys
import tempfile
import time

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS = os.path.join(ROOT, "compat", "corpus")
MANIFEST = os.path.join(ROOT, "compat", "manifest.yaml")
DPI = 100


def manifest():
    files, cur = [], None
    for line in open(MANIFEST, encoding="utf-8"):
        if line.startswith("- id:"):
            cur = {"id": line.split(":", 1)[1].strip()}
            files.append(cur)
        elif cur is not None and line.startswith("  ") and ":" in line:
            k, v = line.strip().split(":", 1)
            cur[k] = v.strip().strip("'")
    return files


def pick(files, per_category):
    """Up to `per_category` passing, unencrypted files per category,
    distinct producers first, in id order (deterministic)."""
    out = []
    by_cat = collections.defaultdict(list)
    for f in sorted(files, key=lambda f: f["id"]):
        if f.get("expect") == "pass" and f.get("encrypted") == "false" and f["category"] != "encrypted":
            by_cat[f["category"]].append(f)
    for cat in sorted(by_cat):
        seen, first, rest = set(), [], []
        for f in by_cat[cat]:
            (rest if f.get("producer") in seen else first).append(f)
            seen.add(f.get("producer"))
        out.extend((first + rest)[:per_category])
    return out


def path_of(f):
    for name in (f["id"] + ".pdf", f["id"]):
        p = os.path.join(CORPUS, name)
        if os.path.isfile(p):
            return p
    return None


def plain_geometry(pdf):
    """(width, height) when page 1 is plain MediaBox space, else None."""
    r = subprocess.run(["pdfinfo", "-box", "-f", "1", "-l", "1", pdf], capture_output=True, text=True)
    if r.returncode != 0:
        return None
    rot = re.search(r"Page\s+1 rot:\s+(\d+)", r.stdout)
    mb = re.search(r"Page\s+1 MediaBox:\s+([-\d.]+)\s+([-\d.]+)\s+([-\d.]+)\s+([-\d.]+)", r.stdout)
    cb = re.search(r"Page\s+1 CropBox:\s+([-\d.]+)\s+([-\d.]+)\s+([-\d.]+)\s+([-\d.]+)", r.stdout)
    if not (rot and mb and cb) or rot.group(1) != "0":
        return None
    m = [float(v) for v in mb.groups()]
    c = [float(v) for v in cb.groups()]
    if m != c or m[0] != 0 or m[1] != 0:
        return None
    return m[2], m[3]


def words(pdf, height):
    """Page-1 words as (text, llx, lly, urx, ury) in PDF space."""
    r = subprocess.run(["pdftotext", "-bbox", "-f", "1", "-l", "1", pdf, "-"], capture_output=True, text=True)
    out = []
    for m in re.finditer(r'<word xMin="([\d.]+)" yMin="([\d.]+)" xMax="([\d.]+)" yMax="([\d.]+)">([^<]*)</word>', r.stdout):
        x0, y0, x1, y1 = (float(v) for v in m.groups()[:4])
        out.append((html.unescape(m.group(5)), x0, height - y1, x1, height - y0))
    return out


def rects_for(pdf, size):
    w, h = size
    ws = words(pdf, h)
    rects = []
    long_words = [x for x in ws if sum(c.isalpha() for c in x[0]) >= 4 and x[3] - x[1] > 1 and x[4] - x[2] > 1]
    if long_words:
        t = sorted(long_words, key=lambda x: abs((x[2] + x[4]) / 2 - h / 2))[0]
        rects.append(("word", (t[1] - 0.5, t[2] - 0.5, t[3] + 0.5, t[4] + 0.5)))
        rects.append(("line", (0.0, t[2] - 0.5, w, t[4] + 0.5)))
    rects.append(("block", (w / 3, h / 3, 2 * w / 3, 2 * h / 3)))
    return rects, ws


def mupdf_chars(pdf, size):
    """Page-1 characters as (char, origin x, origin y, llx, lly, urx, ury)
    in the page space Poppler reports (`size`), from MuPDF (`mutool draw
    -F stext`) — a second text engine, character by character. MuPDF
    applies /UserUnit, so its coordinates are scaled back. None when
    mutool is not installed."""
    try:
        r = subprocess.run(["mutool", "draw", "-q", "-F", "stext", "-o", "-", pdf, "1"],
                           capture_output=True, text=True)
    except FileNotFoundError:
        return None
    if r.returncode != 0:
        return None
    m = re.search(r'<page id="[^"]*" width="([^"]+)" height="([^"]+)"', r.stdout)
    k = size[0] / float(m.group(1)) if m and float(m.group(1)) > 0 else 1.0
    height = size[1]
    out = []
    for m in re.finditer(r'<char c="([^"]*)" quad="([^"]+)" x="([^"]+)" y="([^"]+)"', r.stdout):
        q = [float(v) * k for v in m.group(2).split()]
        xs, ys = q[0::2], q[1::2]
        out.append((html.unescape(m.group(1)), float(m.group(3)) * k, height - float(m.group(4)) * k,
                    min(xs), height - max(ys), max(xs), height - min(ys)))
    return out


def image_digests(pdf, tmp, tag):
    """Digests of page 1's images as stored (`pdfimages -all`)."""
    import hashlib
    d = os.path.join(tmp, "img-" + tag)
    os.makedirs(d, exist_ok=True)
    subprocess.run(["pdfimages", "-all", "-f", "1", "-l", "1", pdf, os.path.join(d, "i")], capture_output=True)
    return collections.Counter(hashlib.sha256(open(os.path.join(d, f), "rb").read()).hexdigest()
                               for f in sorted(os.listdir(d)))


def render(pdf, base):
    subprocess.run(["pdftoppm", "-r", str(DPI), "-gray", "-f", "1", "-l", "1", "-singlefile", pdf, base],
                   capture_output=True, check=True)
    data = open(base + ".pgm", "rb").read()
    parts = data.split(maxsplit=4)
    return int(parts[1]), int(parts[2]), parts[4]


def pixel_checks(inp, out, rect, size, tmp):
    w0, h0, a = render(inp, os.path.join(tmp, "in"))
    w1, h1, b = render(out, os.path.join(tmp, "out"))
    if (w0, h0) != (w1, h1):
        return None, None, None
    s = DPI / 72.0
    _, ph = size
    x0, x1 = rect[0] * s, rect[2] * s
    y0, y1 = (ph - rect[3]) * s, (ph - rect[1]) * s
    left_ink, changed_out, left_black = 0, 0, 0
    for y in range(h0):
        row = y * w0
        for x in range(w0):
            i = row + x
            inside = x0 + 2 <= x <= x1 - 2 and y0 + 2 <= y <= y1 - 2
            outside = x < x0 - 2 or x > x1 + 2 or y < y0 - 2 or y > y1 + 2
            if inside and b[i] != 255 and a[i] == b[i]:
                left_ink += 1
                left_black += b[i] == 0
            elif outside and a[i] != b[i]:
                changed_out += 1
    return left_ink, changed_out, left_black


def ink_not_from_images(inp, out, rect, size, tmp):
    """Unchanged non-white pixels inside the rect that stay when no image
    of the output is drawn (each image XObject turned into an empty
    form, so blending sees nothing either): ink that is not image
    samples. `None` when the output cannot be rewritten so."""
    plain = os.path.join(tmp, "plain.pdf")
    q = subprocess.run(["qpdf", "--qdf", "--object-streams=disable", out, plain], capture_output=True)
    if q.returncode not in (0, 3) or not os.path.exists(plain):
        return None
    data = open(plain, "rb").read()
    parts, pos = [], 0
    for m in re.finditer(rb"(\d+ 0 obj\n<<)(.*?)(>>\nstream\n)", data, re.S):
        if not re.search(rb"/Subtype /Image\b", m.group(2)):
            continue
        end = data.find(b"\nendstream", m.end())
        if end < 0:
            return None
        d = re.sub(rb"/Subtype /Image\b", b"/Subtype /Form /BBox [0 0 1 1]", m.group(2))
        d = re.sub(rb"/(Filter|DecodeParms) (/\w+|\[[^\]]*\])", b"", d)
        parts += [data[pos:m.start()], m.group(1), d, m.group(3), b" " * (end - m.end())]
        pos = end
    parts.append(data[pos:])
    blank, fixed = os.path.join(tmp, "blank.pdf"), os.path.join(tmp, "blank-fixed.pdf")
    open(blank, "wb").write(b"".join(parts))
    fx = subprocess.run(["fix-qdf", blank], capture_output=True)
    if fx.returncode != 0:
        return None
    open(fixed, "wb").write(fx.stdout)
    left, _, _ = pixel_checks(inp, fixed, rect, size, tmp)
    return left


def within(word, r, pad=0.0):
    return word[1] >= r[0] - pad and word[3] <= r[2] + pad and word[2] >= r[1] - pad and word[4] <= r[3] + pad


def clear_of(word, r, pad):
    return word[3] < r[0] - pad or word[1] > r[2] + pad or word[4] < r[1] - pad or word[2] > r[3] + pad


def run_one(kasane, pdf, kind, rect, size, in_words, tmp, extra_args=()):
    out = os.path.join(tmp, "out.pdf")
    if os.path.exists(out):
        os.remove(out)
    args = [kasane, "redact", pdf, "--page", "1", "--rect"] + [f"{v:.3f}" for v in rect] + list(extra_args) + ["--output", out]
    t0 = time.monotonic()
    r = subprocess.run(args, capture_output=True, text=True, timeout=600)
    res = {"kind": kind, "rect": [round(v, 2) for v in rect], "seconds": round(time.monotonic() - t0, 3)}
    if r.returncode != 0:
        msg = (r.stderr or r.stdout).strip().splitlines()[-1] if (r.stderr or r.stdout).strip() else ""
        m = re.match(r"error \[unsupported feature\]: unsupported PDF feature: ([^(]*?)\s*(\(.*)?$", msg)
        if m and os.path.exists(out) is False and "failed verification" not in m.group(1):
            reason = m.group(1).strip()
            detail = (m.group(2) or "").strip("() ")
            if reason.endswith("with blockers"):
                # The blocker kind: the text before its own detail.
                reason += ": " + re.split(r"[:—(]", detail, maxsplit=1)[0].strip()
            res.update(outcome="refused", reason=reason, detail=detail[:200])
        elif (k := re.match(r"error \[([^\]]+)\]", msg)) \
                and r.returncode == 1 and not os.path.exists(out):
            # Any other typed error (malformed input, a limit, a
            # signature or encryption policy): no output, nothing claimed.
            res.update(outcome="refused", reason="other: " + k.group(1), detail=msg[:200])
        else:
            res.update(outcome="unexpected", reason=msg[:200])
        return res
    problems = []
    if not os.path.exists(out):
        problems.append("success without an output file")
        res.update(outcome="unexpected", reason="; ".join(problems))
        return res
    q = subprocess.run(["qpdf", "--check", out], capture_output=True, text=True)
    if q.returncode not in (0, 3):
        problems.append("qpdf --check failed")
    m = re.search(r"(\d+) finding", r.stdout)
    res["elsewhere"] = sum(1 for l in r.stdout.splitlines() if " also shown on page " in l or " also stored in object " in l)
    out_words = words(out, size[1])
    leaked = [w for w in out_words if within(w, rect, -0.5)]
    if leaked:
        problems.append(f"{len(leaked)} word(s) still extracted inside the rect")
    kept_in = collections.Counter(w[0] for w in in_words if clear_of(w, rect, 1.0))
    kept_out = collections.Counter(w[0] for w in out_words)
    lost = sum((kept_in - kept_out).values())
    res["outside_words"] = sum(kept_in.values())
    res["outside_words_lost"] = lost
    # The same by character: pdftotext may regroup kept glyphs into
    # other words once a neighbour is removed (per-glyph words in
    # vertical CJK text); characters do not depend on the grouping.
    chars_in = collections.Counter(c for w in in_words if clear_of(w, rect, 1.0) for c in w[0])
    chars_out = collections.Counter(c for w in out_words if not within(w, rect, -0.5) for c in w[0])
    res["outside_chars"] = sum(chars_in.values())
    res["outside_chars_lost"] = sum((chars_in - chars_out).values())
    # Character by character with MuPDF: kept characters clear of the
    # rect must be found again at the same origin (to 0.01 pt) — a
    # word regrouped by pdftotext is not a loss, a character gone or
    # moved is; no character may be extracted inside the rect.
    cin = mupdf_chars(pdf, size)
    cout = mupdf_chars(out, size) if cin is not None else None
    if cin is not None and cout is not None:
        boxed = lambda c: (c[0], c[3], c[4], c[5], c[6])
        kept = [c for c in cin if c[0].strip() and clear_of(boxed(c), rect, 1.0)]
        index = collections.defaultdict(list)
        for c in cout:
            index[c[0]].append((c[1], c[2]))
        lost = moved = 0
        for c in kept:
            near = index.get(c[0], [])
            if any(abs(x - c[1]) <= 0.01 and abs(y - c[2]) <= 0.01 for x, y in near):
                continue
            if any(abs(x - c[1]) <= 1 and abs(y - c[2]) <= 1 for x, y in near):
                moved += 1
            else:
                lost += 1
        inside = [c for c in cout if c[0].strip() and within(boxed(c), rect, -0.5)]
        res["outside_chars_mupdf"] = len(kept)
        res["outside_chars_mupdf_lost"] = lost
        res["outside_chars_mupdf_moved"] = moved
        res["words_regrouped"] = max(0, res["outside_words_lost"] - res["outside_chars_lost"])
        if inside:
            problems.append(f"{len(inside)} character(s) still extracted inside the rect (MuPDF)")
    left_ink, changed_out, left_black = pixel_checks(pdf, out, rect, size, tmp)
    if left_ink is None:
        problems.append("render size changed")
    else:
        res["outside_pixels_changed"] = changed_out
        res["unchanged_ink_inside"] = left_ink
        if left_ink:
            problems.append(f"{left_ink} unchanged non-white pixel(s) inside the rect")
    for k in ("codes removed", "images edited", "forms edited", "paths removed", "paths cut at a rect",
              "shadings removed"):
        mm = re.search(re.escape(k) + r": (\d+)", r.stdout)
        if mm:
            res[k.replace(" ", "_")] = int(mm.group(1))
    # An image reported edited must not come out with every image of the
    # page unchanged (an edit lost on the way), unless an original is
    # reported still drawn elsewhere.
    if res.get("images_edited"):
        di, do = image_digests(pdf, tmp, "in"), image_digests(out, tmp, "out")
        res["images_unchanged"] = sum((di & do).values())
        if di and not (di - do) and not res.get("elsewhere"):
            problems.append("an image was reported edited but every image of the page is unchanged")
    # The one known false signal: unchanged ink inside the rect that is
    # all black where images were zeroed (samples already black look
    # unchanged). Still unexpected — black content left over a zeroed
    # image would look the same — but marked for the hand check.
    # (Zeroed samples under a soft mask or translucent paint look grey,
    # not black: the mechanical check below decides them alike.)
    if left_ink and res.get("images_edited") and len(problems) == 1:
        res["check_by_hand"] = (
            "unchanged ink inside is all black where images were zeroed"
            if left_ink == left_black
            else "unchanged ink inside where images were zeroed")
        # Checked mechanically: with no image drawn, does any of that
        # ink stay? None staying means it is all image samples —
        # which `redact` verifies are zero inside the rect. The run stays
        # `unexpected`; the check only says what the ink is.
        rest = ink_not_from_images(pdf, out, rect, size, tmp)
        res["unchanged_ink_not_from_images"] = rest
        if rest == 0:
            res["check_by_hand"] = ("verified: the unchanged ink inside is all image samples "
                                    "(zero inside the rect)")
    if problems:
        res.update(outcome="unexpected", reason="; ".join(problems))
    else:
        res["outcome"] = "ok"
    return res


def main():
    if len(sys.argv) < 2:
        sys.exit(__doc__)
    kasane = os.path.abspath(sys.argv[1])
    per_category = 4
    out_json = None
    extra_args = []
    a = sys.argv[2:]
    while a:
        if a[0] == "--per-category":
            per_category = int(a[1]); a = a[2:]
        elif a[0] == "--json":
            out_json = a[1]; a = a[2:]
        elif a[0] == "--redact-arg":
            extra_args.append(a[1]); a = a[2:]
        else:
            sys.exit(f"unknown argument {a[0]}")
    for tool in ("qpdf", "pdftotext", "pdftoppm", "pdfinfo"):
        if subprocess.run(["which", tool], capture_output=True).returncode != 0:
            sys.exit(f"{tool} not found")
    results = []
    skipped = []
    for f in pick(manifest(), per_category):
        pdf = path_of(f)
        if pdf is None:
            skipped.append((f["id"], "not fetched"))
            continue
        size = plain_geometry(pdf)
        if size is None:
            skipped.append((f["id"], "page geometry not plain MediaBox space"))
            continue
        rects, in_words = rects_for(pdf, size)
        for kind, rect in rects:
            with tempfile.TemporaryDirectory() as tmp:
                res = run_one(kasane, pdf, kind, rect, size, in_words, tmp, extra_args)
            res.update(id=f["id"], category=f["category"], producer=f.get("producer"))
            results.append(res)
            extra = res.get("reason", "")
            print(f"{res['outcome']:<10} {f['category']:<12} {f['id'][:40]:<40} {kind:<5} {extra[:110]}")
    counts = collections.Counter(r["outcome"] for r in results)
    reasons = collections.Counter(r["reason"] for r in results if r["outcome"] == "refused")
    files_ok = len({r["id"] for r in results if r["outcome"] == "ok"})
    print()
    print(f"files: {len({r['id'] for r in results})} evaluated, {len(skipped)} skipped; runs: {len(results)}")
    for k in ("ok", "refused", "unexpected"):
        print(f"  {k:<10} {counts.get(k, 0)}")
    print(f"  files with at least one success: {files_ok}")
    oks = [r for r in results if r["outcome"] == "ok"]
    if oks:
        print(f"  successes: outside words lost {sum(r.get('outside_words_lost', 0) for r in oks)}"
              f" of {sum(r.get('outside_words', 0) for r in oks)}"
              f" (characters lost {sum(r.get('outside_chars_lost', 0) for r in oks)}"
              f" of {sum(r.get('outside_chars', 0) for r in oks)});"
              f" runs with outside pixels changed {sum(1 for r in oks if r.get('outside_pixels_changed'))};"
              f" MuPDF characters lost {sum(r.get('outside_chars_mupdf_lost', 0) for r in oks)},"
              f" moved {sum(r.get('outside_chars_mupdf_moved', 0) for r in oks)}"
              f" of {sum(r.get('outside_chars_mupdf', 0) for r in oks)};"
              f" words only regrouped {sum(r.get('words_regrouped', 0) for r in oks)};"
              f" runs reporting copies elsewhere {sum(1 for r in oks if r.get('elsewhere'))}")
    secs = sorted(r.get("seconds", 0) for r in results)
    if secs:
        print(f"  redact time: total {sum(secs):.1f} s, median {secs[len(secs) // 2]:.2f} s, max {secs[-1]:.2f} s")
    hand = [r for r in results if r.get("check_by_hand")]
    for r in hand:
        print(f"  check by hand: {r['id']} {r['kind']}: {r['check_by_hand']}")
    print("refusal reasons:")
    for reason, n in reasons.most_common():
        print(f"  {n:>3}  {reason}")
    for i, why in skipped:
        print(f"skipped {i}: {why}")
    if out_json:
        json.dump({"results": results, "skipped": skipped}, open(out_json, "w"), indent=1)
    sys.exit(1 if counts.get("unexpected") else 0)


if __name__ == "__main__":
    main()
