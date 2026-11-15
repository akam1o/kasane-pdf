#!/usr/bin/env python3
"""Tests for the text measurements of the redaction corpus evaluation
(V1 in docs/compatibility-roadmap.md): scripts/eval-redact-corpus.py's
Poppler and MuPDF measurements and their states, and
scripts/check-redact-corpus.py's handling of them. No corpus.

    python3 scripts/test_redact_text_measure.py           (PR: fake tools)
    python3 scripts/test_redact_text_measure.py --real    (Full Gate)

The default run replaces `mutool` and `pdftotext` with fake commands
(every way a tool can fail) and the run's tools with stubs, so that
each state and each outcome is known in advance. --real runs the
installed MuPDF and Poppler on synthetic PDFs (tests/... helpers of
scripts/verify-redact.py) and checks the state each case must have;
it also prints, for the record, MuPDF's version and the attribute
names of its first <char> element (no text).
"""
import contextlib
import importlib.util
import io
import json
import os
import stat
import subprocess
import sys
import tempfile
import unittest
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)


def load(name, file):
    spec = importlib.util.spec_from_file_location(name, os.path.join(HERE, file))
    mod = importlib.util.module_from_spec(spec)
    argv, sys.argv = sys.argv, [sys.argv[0]]
    try:
        spec.loader.exec_module(mod)
    finally:
        sys.argv = argv
    return mod


ev = load("eval_redact_corpus", "eval-redact-corpus.py")

STEXT_1_28 = (b'<?xml version="1.0"?>\n<document name="x.pdf">\n<page id="page1" width="200" height="200">\n'
              b'<block bbox="10 10 50 22">\n<line bbox="10 10 50 22" wmode="0" dir="1 0">\n'
              b'<font name="F" size="12">\n'
              b'<char c="A" quad="10 10 18 10 10 22 18 22" x="10" y="20" bidi="0" color="#000000" alpha="#ff" flags="0"/>\n'
              b'<char c="&amp;" quad="18 10 26 10 18 22 26 22" x="18" y="20" bidi="0" color="#000000" alpha="#ff" flags="0"/>\n'
              b'</font>\n</line>\n</block>\n</page>\n</document>\n')
# The same page with the attributes of <char> in another order (as
# other MuPDF versions write it): it must read identically.
STEXT_REORDERED = STEXT_1_28.replace(
    b'<char c="A" quad="10 10 18 10 10 22 18 22" x="10" y="20"',
    b'<char quad="10 10 18 10 10 22 18 22" x="10" y="20" c="A"').replace(
    b'<char c="&amp;" quad="18 10 26 10 18 22 26 22" x="18" y="20"',
    b'<char quad="18 10 26 10 18 22 26 22" x="18" y="20" c="&amp;"')
STEXT_EMPTY = b'<?xml version="1.0"?>\n<document name="x.pdf">\n<page id="page1" width="200" height="200">\n</page>\n</document>\n'
BBOX = (b'<!DOCTYPE html><html><head></head><body>\n<doc>\n  <page width="200.000000" height="200.000000">\n'
        b'    <word xMin="10.000000" yMin="10.000000" xMax="50.000000" yMax="22.000000">PUBLIC</word>\n'
        b'  </page>\n</doc>\n</body></html>\n')
BBOX_EMPTY = (b'<!DOCTYPE html><html><head></head><body>\n<doc>\n  <page width="200.000000" height="200.000000">\n'
              b'  </page>\n</doc>\n</body></html>\n')

FAKE = r'''#!{python}
import os, signal, sys, time
me = os.path.basename(sys.argv[0])
mode = os.environ.get("FAKE_MODE_" + me, "ok")
out = open(os.environ["FAKE_OUT_" + me], "rb").read() if os.environ.get("FAKE_OUT_" + me) else b""
if mode == "exit1":
    sys.stderr.write("error\n"); sys.exit(1)
if mode == "timeout":
    time.sleep(30)
if mode == "crash":
    os.kill(os.getpid(), signal.SIGSEGV)
if mode == "stderr_only":
    sys.stderr.write("warning: something\n"); sys.exit(0)
if mode == "nothing":
    sys.exit(0)
sys.stdout.buffer.write(out)
'''


class FakeTool:
    """A fake `mutool` / `pdftotext` writing a given output, or failing."""

    def __init__(self, d, name):
        self.path = os.path.join(d, name)
        with open(self.path, "w") as fh:
            fh.write(FAKE.format(python=sys.executable))
        os.chmod(self.path, os.stat(self.path).st_mode | stat.S_IEXEC)
        self.dir = d
        self.name = name

    @contextlib.contextmanager
    def __call__(self, mode="ok", output=b""):
        f = os.path.join(self.dir, self.name + ".out")
        with open(f, "wb") as fh:
            fh.write(output)
        mk, ok = f"FAKE_MODE_{self.name}", f"FAKE_OUT_{self.name}"
        old = {k: os.environ.get(k) for k in (mk, ok)}
        os.environ.update({mk: mode, ok: f})
        try:
            yield
        finally:
            for k, v in old.items():
                if v is None:
                    os.environ.pop(k, None)
                else:
                    os.environ[k] = v


class Measurements(unittest.TestCase):
    """The states of one measurement (Phase F 1–13)."""

    @classmethod
    def setUpClass(cls):
        cls.tmp = tempfile.TemporaryDirectory()
        cls.mutool = FakeTool(cls.tmp.name, "mutool")
        cls.pdftotext = FakeTool(cls.tmp.name, "pdftotext")
        os.environ["KASANE_EVAL_MUTOOL"] = cls.mutool.path
        os.environ["KASANE_EVAL_PDFTOTEXT"] = cls.pdftotext.path
        cls.old_timeout = ev.TOOL_TIMEOUT

    @classmethod
    def tearDownClass(cls):
        os.environ.pop("KASANE_EVAL_MUTOOL", None)
        os.environ.pop("KASANE_EVAL_PDFTOTEXT", None)
        cls.tmp.cleanup()

    def mu(self, mode="ok", output=b""):
        with self.mutool(mode, output):
            m = ev.mupdf_chars("x.pdf", (200, 200))
        return m

    def pop(self, mode="ok", output=b""):
        with self.pdftotext(mode, output):
            m = ev.poppler_words("x.pdf", 200)
        return m

    def test_success_in_either_attribute_order(self):
        a, b = self.mu(output=STEXT_1_28), self.mu(output=STEXT_REORDERED)
        self.assertEqual(a["state"], "success")
        self.assertEqual(a["chars"], b["chars"])
        self.assertEqual([c[0] for c in a["chars"]], ["A", "&"])
        # Origin converted to PDF space (y up).
        self.assertEqual(a["chars"][0][1:3], (10.0, 180.0))

    def test_1_a_page_without_text_is_empty_not_a_failure(self):
        self.assertEqual(self.mu(output=STEXT_EMPTY)["state"], "empty")
        self.assertEqual(self.pop(output=BBOX_EMPTY)["state"], "empty")
        self.assertEqual(self.pop(output=BBOX)["state"], "success")

    def test_failures(self):
        ev.TOOL_TIMEOUT = 2
        ev.run_tool.__defaults__ = (2,)
        try:
            cases = {
                "2 exit status not zero": ("exit1", b"", ("error", "exit_status")),
                "3 timeout": ("timeout", b"", ("timeout", "timeout")),
                "4 no output at all": ("nothing", b"", ("incomplete", "no_output")),
                "5 an empty output (whitespace)": ("ok", b"\n \n", ("incomplete", "no_output")),
                "6 a corrupt output": ("ok", STEXT_1_28[:120], ("incomplete", "corrupt_output")),
                "7 standard error only": ("stderr_only", b"", ("incomplete", "no_output")),
                "8 a crash": ("crash", b"", ("error", "crashed")),
                "9 no page": ("ok", b"<document/>", ("incomplete", "no_page")),
                "9 two pages for one": ("ok", STEXT_1_28.replace(b"</page>", b"</page><page id='p2' width='1' height='1'></page>"),
                                        ("incomplete", "page_count")),
                "11 a character without its position": ("ok", STEXT_1_28.replace(b' x="10"', b""), ("incomplete", "char_fields")),
                "11 a position that is not a number": ("ok", STEXT_1_28.replace(b' x="10"', b' x="ten"'),
                                                       ("incomplete", "char_fields")),
                "12 a quad of the wrong length": ("ok", STEXT_1_28.replace(b'quad="10 10 18 10 10 22 18 22"', b'quad="1 2"'),
                                                  ("incomplete", "char_fields")),
                "12 a page without a width": ("ok", STEXT_1_28.replace(b' width="200"', b""), ("incomplete", "page_size")),
                "13 NaN": ("ok", STEXT_1_28.replace(b' x="10"', b' x="nan"'), ("incomplete", "char_fields")),
                "13 Infinity": ("ok", STEXT_1_28.replace(b' y="20"', b' y="inf"'), ("incomplete", "char_fields")),
                "13 an infinite page width": ("ok", STEXT_1_28.replace(b' width="200"', b' width="inf"'),
                                              ("incomplete", "page_size")),
            }
            for name, (mode, out, want) in cases.items():
                with self.subTest(name):
                    m = self.mu(mode, out)
                    self.assertEqual((m["state"], m["reason"]), want, name)
                    self.assertNotIn("chars", m)
            # The same for Poppler.
            for mode, out, want in (("exit1", b"", ("error", "exit_status")), ("timeout", b"", ("timeout", "timeout")),
                                    ("crash", b"", ("error", "crashed")), ("stderr_only", b"", ("incomplete", "no_output")),
                                    ("ok", b"<doc></doc>", ("incomplete", "no_page")),
                                    ("ok", BBOX.replace(b'xMin="10.000000"', b'xMin="nan"'), ("incomplete", "char_fields")),
                                    ("ok", b"\xff\xfe<page>", ("incomplete", "encoding"))):
                with self.subTest(f"poppler {mode} {want}"):
                    m = self.pop(mode, out)
                    self.assertEqual((m["state"], m["reason"]), want)
        finally:
            ev.TOOL_TIMEOUT = self.old_timeout
            ev.run_tool.__defaults__ = (self.old_timeout,)

    def test_a_tool_not_installed(self):
        os.environ["KASANE_EVAL_MUTOOL"] = os.path.join(self.tmp.name, "absent")
        try:
            m = ev.mupdf_chars("x.pdf", (200, 200))
        finally:
            os.environ["KASANE_EVAL_MUTOOL"] = self.mutool.path
        self.assertEqual((m["state"], m["reason"]), ("error", "not_installed"))

    def test_mupdf_empty_where_poppler_has_text_is_incomparable(self):
        with self.mutool(output=STEXT_EMPTY), self.pdftotext(output=BBOX):
            pop, mu = ev.input_measures("x.pdf", (200, 200))
        self.assertEqual(pop["state"], "success")
        self.assertEqual((mu["state"], mu["reason"]), ("incomparable", "empty_while_poppler_has_text"))
        # Both empty: a page without text, measured.
        with self.mutool(output=STEXT_EMPTY), self.pdftotext(output=BBOX_EMPTY):
            pop, mu = ev.input_measures("x.pdf", (200, 200))
        self.assertEqual((pop["state"], mu["state"]), ("empty", "empty"))
        # MuPDF has text where Poppler has none: both measured (a page
        # Poppler cannot read the text of is not MuPDF's failure).
        with self.mutool(output=STEXT_1_28), self.pdftotext(output=BBOX_EMPTY):
            pop, mu = ev.input_measures("x.pdf", (200, 200))
        self.assertEqual((pop["state"], mu["state"]), ("empty", "success"))


def m_ok(words=None, chars=None):
    if chars is not None:
        return ev.measure("success" if chars else "empty", chars=chars)
    return ev.measure("success" if words else "empty", words=words or [])


WORD = ("PUBLIC", 10.0, 178.0, 50.0, 190.0)          # clear of RECT
CHAR = ("P", 10.0, 180.0, 10.0, 178.0, 18.0, 190.0)  # clear of RECT
RECT = (100.0, 100.0, 150.0, 150.0)


class RunOutcomes(unittest.TestCase):
    """One run's outcome from its measurements (Phase F 9, 10, 14–16)."""

    def run_one(self, in_pop, in_mu, out_pop, out_mu, pages=(1, 1)):
        stubs = {
            "poppler_words": lambda pdf, h: out_pop,
            "mupdf_chars": lambda pdf, size: out_mu,
            "page_count": lambda pdf: pages[0] if pdf.endswith("in.pdf") else pages[1],
            "pixel_checks": lambda *a: (0, 0, 0),
        }
        old = {k: getattr(ev, k) for k in stubs}
        real_run = ev.subprocess.run

        def fake_run(argv, *a, **k):
            if argv[1:2] == ["redact"]:
                with open(argv[-1], "wb") as fh:
                    fh.write(b"%PDF-1.7\n")
                return subprocess.CompletedProcess(argv, 0, stdout="", stderr="")
            return subprocess.CompletedProcess(argv, 0, stdout="", stderr="")

        try:
            for k, v in stubs.items():
                setattr(ev, k, v)
            ev.subprocess.run = fake_run
            with tempfile.TemporaryDirectory() as tmp:
                return ev.run_one("kasane", os.path.join(tmp, "in.pdf"), "block", RECT, (200, 200), in_pop, in_mu, tmp)
        finally:
            for k, v in old.items():
                setattr(ev, k, v)
            ev.subprocess.run = real_run

    def test_all_measured(self):
        r = self.run_one(m_ok([WORD]), m_ok(chars=[CHAR]), m_ok([WORD]), m_ok(chars=[CHAR]))
        self.assertEqual(r["outcome"], "ok", r)
        self.assertEqual(r["text_measures"], {k: "success" for k in ("poppler_in", "poppler_out", "mupdf_in", "mupdf_out")})
        self.assertEqual((r["outside_chars_mupdf"], r["outside_chars_mupdf_lost"]), (1, 0))

    def test_a_page_without_text_is_ok(self):
        r = self.run_one(m_ok(), m_ok(chars=[]), m_ok(), m_ok(chars=[]))
        self.assertEqual(r["outcome"], "ok", r)
        self.assertEqual(r["outside_chars_mupdf"], 0)

    def test_14_poppler_measured_mupdf_failed(self):
        r = self.run_one(m_ok([WORD]), m_ok(chars=[CHAR]), m_ok([WORD]), ev.measure("error", "exit_status"))
        self.assertEqual(r["outcome"], "incomplete", r)
        self.assertIn("mupdf_out", r["reason"])
        # Poppler's own measurement is kept as it is.
        self.assertEqual((r["outside_words"], r["outside_words_lost"]), (1, 0))
        self.assertNotIn("outside_chars_mupdf", r)

    def test_15_poppler_failed_mupdf_measured(self):
        r = self.run_one(m_ok([WORD]), m_ok(chars=[CHAR]), ev.measure("timeout", "timeout"), m_ok(chars=[CHAR]))
        self.assertEqual(r["outcome"], "incomplete", r)
        self.assertNotIn("outside_words", r)
        self.assertEqual(r["outside_chars_mupdf"], 1)

    def test_16_both_failed(self):
        r = self.run_one(ev.measure("error", "crashed"), ev.measure("incomparable", "empty_while_poppler_has_text"),
                         ev.measure("error", "crashed"), ev.measure("incomplete", "corrupt_output"))
        self.assertEqual(r["outcome"], "incomplete", r)
        self.assertEqual(r["text_measure_reasons"]["mupdf_in"], "empty_while_poppler_has_text")

    def test_a_loss_is_still_found(self):
        # The output lost every character: measured, a loss, not "empty".
        r = self.run_one(m_ok([WORD]), m_ok(chars=[CHAR]), m_ok(), m_ok(chars=[]))
        self.assertEqual(r["outcome"], "ok", r)
        self.assertEqual((r["outside_words_lost"], r["outside_chars_mupdf_lost"]), (1, 1))

    def test_a_leak_found_by_mupdf_alone(self):
        inside = ("S", 120.0, 120.0, 118.0, 118.0, 126.0, 130.0)
        r = self.run_one(m_ok([WORD]), m_ok(chars=[CHAR]), m_ok([WORD]), m_ok(chars=[CHAR, inside]))
        self.assertEqual(r["outcome"], "unexpected", r)

    def test_10_page_count(self):
        r = self.run_one(m_ok([WORD]), m_ok(chars=[CHAR]), m_ok([WORD]), m_ok(chars=[CHAR]), pages=(2, 1))
        self.assertEqual(r["outcome"], "unexpected", r)
        r = self.run_one(m_ok([WORD]), m_ok(chars=[CHAR]), m_ok([WORD]), m_ok(chars=[CHAR]), pages=(None, 1))
        self.assertEqual(r["outcome"], "incomplete", r)


def ok_run(fid="f", kind="word", **m):
    r = {"id": fid, "kind": kind, "category": "c", "outcome": "ok",
         "text_measures": {k: "success" for k in ("poppler_in", "poppler_out", "mupdf_in", "mupdf_out")}}
    r.update({k: 1 for k in ("outside_words", "outside_chars", "outside_chars_mupdf", "outside_pixels_changed")})
    r.update({k: 0 for k in ("outside_words_lost", "outside_chars_lost", "outside_chars_mupdf_lost",
                             "outside_chars_mupdf_moved", "unchanged_ink_inside")})
    r.update(m)
    return r


class Comparison(unittest.TestCase):
    """check-redact-corpus.py (Phase F 11–13, 17–20)."""

    def compare(self, old, new, raw_new=None):
        with tempfile.TemporaryDirectory() as d:
            with open(os.path.join(d, "eval-default.json"), "w") as fh:
                json.dump({"results": old}, fh)
            with open(os.path.join(d, "new.json"), "w") as fh:
                fh.write(raw_new if raw_new is not None else json.dumps({"results": new}))
            r = subprocess.run([sys.executable, os.path.join(HERE, "check-redact-corpus.py"), "default",
                                os.path.join(d, "new.json"), "--baseline", d, "--json", os.path.join(d, "out.json")],
                               capture_output=True, text=True)
            doc = None
            if os.path.exists(os.path.join(d, "out.json")):
                with open(os.path.join(d, "out.json")) as fh:
                    doc = json.load(fh)
        return r.returncode, doc, r.stdout

    def whats(self, doc):
        return sorted({x["what"] for x in doc["regressions"]})

    def test_same(self):
        rc, doc, out = self.compare([ok_run()], [ok_run()])
        self.assertEqual(rc, 0, out)

    def test_incomplete_is_never_ok(self):
        rc, doc, _ = self.compare([ok_run()], [dict(ok_run(), outcome="incomplete")])
        self.assertEqual(rc, 1)
        self.assertIn("measurement_incomplete", self.whats(doc))
        # Not only after an ok: an incomplete run is a failure on its own.
        rc, doc, _ = self.compare([dict(ok_run(), outcome="refused", reason="x")], [dict(ok_run(), outcome="incomplete")])
        self.assertEqual(rc, 1)

    def test_11_a_count_that_is_not_a_count(self):
        for bad in (-1, "3", None, True):
            rc, doc, _ = self.compare([ok_run()], [ok_run(outside_chars_mupdf=bad)])
            self.assertEqual(rc, 1, bad)
            self.assertIn("measurement_missing", self.whats(doc))

    def test_12_19_a_measurement_missing_or_skipped(self):
        for name, run in (("no text_measures", {k: v for k, v in ok_run().items() if k != "text_measures"}),
                          ("one state missing", ok_run(text_measures={"poppler_in": "success", "poppler_out": "success",
                                                                       "mupdf_in": "success"})),
                          ("a state not a measurement", ok_run(text_measures=dict(ok_run()["text_measures"],
                                                                                  mupdf_out="error"))),
                          ("the MuPDF counts dropped", {k: v for k, v in ok_run().items()
                                                        if not k.startswith("outside_chars_mupdf")})):
            with self.subTest(name):
                rc, doc, _ = self.compare([ok_run()], [run])
                self.assertEqual(rc, 1, name)
        # Even against committed results that never had the measurement.
        old = {k: v for k, v in ok_run().items() if not k.startswith("outside_chars_mupdf") and k != "text_measures"}
        rc, _, _ = self.compare([old], [{k: v for k, v in old.items()}])
        self.assertEqual(rc, 1)

    def test_12_13_invalid_results(self):
        cases = {
            "NaN": json.dumps({"results": [ok_run(outside_chars_mupdf=float("nan"))]}),
            "Infinity": json.dumps({"results": [ok_run(outside_chars_lost=float("inf"))]}),
            "not JSON": '{"results": [',
            "no results": "{}",
            "a run without an id": json.dumps({"results": [{k: v for k, v in ok_run().items() if k != "id"}]}),
            "a run twice": json.dumps({"results": [ok_run(), ok_run()]}),
        }
        for name, raw in cases.items():
            with self.subTest(name):
                rc, _, out = self.compare([ok_run()], None, raw_new=raw)
                self.assertEqual(rc, 2, f"{name}: {out}")
                self.assertIn("INVALID", out)

    def test_17_an_improvement_does_not_offset_a_regression(self):
        old = [ok_run("a"), dict(ok_run("b"), outcome="refused", reason="x")]
        new = [ok_run("a", outside_chars_mupdf_lost=2), ok_run("b")]
        rc, doc, _ = self.compare(old, new)
        self.assertEqual(rc, 1)
        self.assertEqual((len(doc["regressions"]), len(doc["improvements"])), (1, 1))

    def test_18_zeros_behind_a_failed_measurement(self):
        # Losses written as 0 while the measurement did not succeed.
        run = ok_run(outside_chars_mupdf=0, text_measures=dict(ok_run()["text_measures"], mupdf_in="incomparable"))
        rc, doc, _ = self.compare([ok_run(outside_chars_mupdf_lost=0)], [run])
        self.assertEqual(rc, 1)
        self.assertIn("measurement_incomplete", self.whats(doc))

    def test_20_a_run_removed(self):
        rc, doc, _ = self.compare([ok_run("a"), ok_run("b")], [ok_run("a")])
        self.assertEqual(rc, 1)
        self.assertIn("run_missing", self.whats(doc))

    def test_the_projection_keeps_states_and_closes_reasons(self):
        proj = load("projection", "redact-results-projection.py")
        r = ok_run(text_measures=dict(ok_run()["text_measures"], mupdf_in="free text"),
                   text_measure_reasons={"mupdf_in": "some private text", "poppler_in": "timeout"})
        out = proj.eval_run(r)
        self.assertEqual(out["text_measures"]["mupdf_in"], "incomplete")
        self.assertEqual(out["text_measure_reasons"], {"mupdf_in": "not_measured", "poppler_in": "timeout"})
        self.assertNotIn("private", json.dumps(out))
        self.assertEqual(proj.eval_run(dict(ok_run(), outcome="incomplete"))["outcome"], "incomplete")


# ---- the real tools ------------------------------------------------------

def synthetic():
    """(name, pdf bytes, expected Poppler state, expected MuPDF state) of
    the synthetic cases. MuPDF state `not measured` means any state but
    success / empty (the tool must not read it as a page)."""
    v = load("verify_redact", "verify-redact.py")
    text = b"BT /F1 12 Tf 10 100 Td (PUBLICWORD 123) Tj ET"
    multi = v.page(text, kids=b"[3 0 R 8 0 R 9 0 R]", count=3, more={
        8: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        9: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"})
    image = v.stream(b"/Type /XObject /Subtype /Image /Width 2 /Height 2 /ColorSpace /DeviceGray /BitsPerComponent 8",
                     bytes([0, 80, 160, 240]))
    helvetica = {5: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"}
    ap = v.stream(b"/Type /XObject /Subtype /Form /BBox [0 0 100 20] /Resources << /Font << /F1 5 0 R >> >>",
                  b"BT /F1 10 Tf 2 5 Td (ANNOTWORD) Tj ET")
    annot = v.page(b"", page_extra=b"/Annots [8 0 R]", more={
        8: b"<< /Type /Annot /Subtype /FreeText /Rect [50 95 150 115] /DA (/Helv 9 Tf 0 g) /Contents (ANNOTWORD) "
           b"/AP << /N 9 0 R >> >>", 9: ap})
    # Japanese: an embedded OpenType (CFF) font, Identity-H codes, the
    # text in /ToUnicode.
    otf = open(os.path.join(ROOT, "assets", "cjk", "NotoSansJP-Regular.otf"), "rb").read()
    tounicode = (b"/CIDInit /ProcSet findresource begin 12 dict begin begincmap /CMapName /U def "
                 b"1 begincodespacerange <0000> <FFFF> endcodespacerange 2 beginbfchar <0001> <65E5> "
                 b"<0002> <672C> endbfchar endcmap CMapName currentdict /CMap defineresource pop end end")
    jp = v.page(b"BT /J1 20 Tf 10 100 Td <00010002> Tj ET", res=b"/Font << /J1 10 0 R >>", more={
        10: b"<< /Type /Font /Subtype /Type0 /BaseFont /NotoSansJP /Encoding /Identity-H /DescendantFonts [11 0 R] "
            b"/ToUnicode 13 0 R >>",
        11: b"<< /Type /Font /Subtype /CIDFontType0 /BaseFont /NotoSansJP /CIDSystemInfo << /Registry (Adobe) "
            b"/Ordering (Identity) /Supplement 0 >> /FontDescriptor 12 0 R /DW 1000 >>",
        12: b"<< /Type /FontDescriptor /FontName /NotoSansJP /Flags 4 /FontBBox [0 -200 1000 900] /ItalicAngle 0 "
            b"/Ascent 880 /Descent -120 /CapHeight 700 /StemV 80 /FontFile3 14 0 R >>",
        13: v.stream(b"", tounicode),
        14: v.stream(b"/Subtype /OpenType", otf, flate=True)})
    enc = open(os.path.join(ROOT, "tests", "fixtures", "encrypted", "enc_r4_pdfdoc.pdf"), "rb").read()
    good = v.build(v.page(text))
    return [
        ("1 Latin text, embedded TrueType", good, "success", "success"),
        ("2 Japanese, embedded CFF, ToUnicode", v.build(jp), "success", "success"),
        ("3 three pages (page 1 measured)", v.build(multi), "success", "success"),
        ("4 an image only", v.build(v.page(b"q 100 0 0 100 50 50 cm /Im Do Q", res=b"/XObject << /Im 8 0 R >>",
                                            more={8: image})), "empty", "empty"),
        ("5 a blank page", v.build(v.page(b"")), "empty", "empty"),
        # Both draw annotation appearances (Poppler 26 and MuPDF 1.27+).
        ("6 text only in an annotation appearance", v.build(annot), "success", "success"),
        ("7 embedded font (= 1)", good, "success", "success"),
        ("8 a non-embedded font (Helvetica)", v.build(v.page(text, more=helvetica)), "success", "success"),
        ("9 a broken file", good[:200], "not measured", "not measured"),
        ("10 encrypted with a user password MuPDF does not have", enc, "not measured", "not measured"),
    ]


def real():
    for k in ("KASANE_EVAL_MUTOOL", "KASANE_EVAL_PDFTOTEXT"):
        os.environ.pop(k, None)
    v = subprocess.run(["mutool", "-v"], capture_output=True, text=True)
    print("mutool:", (v.stdout + v.stderr).strip().splitlines()[0] if (v.stdout + v.stderr).strip() else "?")
    p = subprocess.run(["pdftotext", "-v"], capture_output=True, text=True)
    print("pdftotext:", (p.stdout + p.stderr).strip().splitlines()[0])
    failed = 0
    with tempfile.TemporaryDirectory() as d:
        for name, pdf, want_pop, want_mu in synthetic():
            path = os.path.join(d, "case.pdf")
            with open(path, "wb") as fh:
                fh.write(pdf)
            pop = ev.poppler_words(path, 200)
            mu = ev.mupdf_chars(path, (200, 200))
            if name.startswith("1 "):
                raw = subprocess.run(["mutool", "draw", "-q", "-F", "stext", "-o", "-", path, "1"],
                                     capture_output=True).stdout
                import xml.etree.ElementTree as ET
                c = next(iter(ET.fromstring(raw).iter("char")), None)
                print("mutool stext <char> attributes, in order:",
                      None if c is None else " ".join(c.attrib))
            ok = True
            for who, m, want in (("poppler", pop, want_pop), ("mupdf", mu, want_mu)):
                good = m["state"] not in ev.MEASURED if want == "not measured" else m["state"] == want
                ok &= good
            # The text, where the case has it, is read correctly — without
            # whitespace, as the evaluation compares it: MuPDF inserts
            # spaces where a glyph's advance differs from its width (1.23).
            read_mu = "".join(c[0] for c in mu.get("chars") or [] if c[0].strip())
            read_pop = "".join(w[0] for w in pop.get("words") or [])
            want_text = {"1": "PUBLICWORD123", "3": "PUBLICWORD123", "7": "PUBLICWORD123", "8": "PUBLICWORD123",
                         "2": "\u65e5\u672c", "6": "ANNOTWORD"}.get(name.split()[0])
            if ok and want_text:
                ok &= read_mu == want_text and read_pop == want_text
            print(f"{'ok  ' if ok else 'FAIL'} {name}: Poppler {pop['state']}/{pop['reason']}, "
                  f"MuPDF {mu['state']}/{mu['reason']} (want {want_pop}, {want_mu})")
            if not ok:
                # Synthetic input: its text may be shown.
                print("     MuPDF read:", repr("".join(c[0] for c in mu.get("chars") or [])),
                      "Poppler read:", repr(" ".join(w[0] for w in pop.get("words") or [])))
            failed += not ok
    print(f"test_redact_text_measure --real: {'failed' if failed else 'passed'} ({failed} failing)")
    return 1 if failed else 0


if __name__ == "__main__":
    if sys.argv[1:2] == ["--real"]:
        sys.exit(real())
    unittest.main()
