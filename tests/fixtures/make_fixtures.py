#!/usr/bin/env python3
"""Generate the hand-crafted PDF fixtures under tests/fixtures/.

The files are intentionally minimal but byte-accurate: xref offsets are
computed, not guessed. Regenerate with:

    python3 tests/fixtures/make_fixtures.py

Fixtures produced:

  hello.pdf            Minimal 1-page document, ASCII-only content stream.
  content_ops.pdf      Content stream exercising graphics-state, path,
                       colour, text and marked-content operators.
  render_rects.pdf     Renderable page: rectangles, strokes, fills,
                       clipping, cm, q/Q — no text/XObject/gs.
  render_annot.pdf     Page with /Annots — render must refuse.
  render_text.pdf      Page with Tj — render must refuse (text is M4+).
  pages.pdf            3 pages in a 2-level page tree; inherited MediaBox
                       on the root, a nested Pages node overriding it,
                       a Rotate on one page, a shared (indirect) MediaBox,
                       and a non-stream /Contents object.
  incremental.pdf      Two xref sections joined by /Prev (simulated
                       incremental update; the new section overrides
                       object 4 with a different MediaBox).
  xref_stream.pdf      A well-formed xref stream (valid syntax, but the
                       feature is unsupported -> expected error).
  encrypted.pdf        Classic xref table whose trailer has /Encrypt.
  bad_offset.pdf       startxref points into the middle of an object.
  missing_ref.pdf      Catalog /Pages references an undefined object.
  cyclic_pages.pdf     A Pages node lists itself in its own Kids.
  truncated.pdf        Ends inside a page object definition.

Generated files are deterministic (no timestamps). The checks in
tests/parsing.rs document the expected outcome of each file.
"""

import os
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))


def build_pdf(objects, trailer_extra="", comment=True):
    """Serialize `objects` (list of (num, bytes)) into a classic-xref PDF.

    Returns the complete file bytes. Object bodies should include the
    trailing newline if a readable file is desired; xref offsets are
    computed from actual positions.
    """
    out = bytearray()
    out += b"%PDF-1.4\n"
    if comment:
        out += b"%\xe2\xe3\xcf\xd3\n"  # binary marker comment
    offsets = {}
    for num, body in objects:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    xref_pos = len(out)
    size = max(offsets) + 1 if offsets else 1
    out += b"xref\n"
    out += f"0 {size}\n".encode()
    out += b"0000000000 65535 f \n"
    for n in range(1, size):
        off = offsets.get(n, 0)
        gen = "00000" if n in offsets else "65535"
        typ = "n" if n in offsets else "f"
        out += f"{off:010d} {gen} {typ} \n".encode()
    out += b"trailer\n"
    out += f"<< /Size {size} /Root 1 0 R{trailer_extra} >>\n".encode()
    out += b"startxref\n"
    out += f"{xref_pos}\n".encode()
    out += b"%%EOF\n"
    return bytes(out)


def write(name, data):
    path = os.path.join(HERE, name)
    with open(path, "wb") as f:
        f.write(data)
    print(f"wrote {name} ({len(data)} bytes)")


def hello():
    stream = b"BT /F1 24 Tf 72 720 Td (Hello, PDF!) Tj ET"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ]
    write("hello.pdf", build_pdf(objects))


def pages():
    shared_box = b"[0 0 400 400]"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        # Root Pages: inherited MediaBox letter, Kids = one leaf + subtree.
        (2, b"<< /Type /Pages /Kids [3 0 R 5 0 R] /Count 3 "
            b"/MediaBox [0 0 612 792] /Resources << >> >>"),
        # Page 1: inherits MediaBox from root; own Rotate.
        (3, b"<< /Type /Page /Parent 2 0 R /Rotate 90 /Contents 8 0 R >>"),
        # (object 4 is the shared MediaBox)
        (4, shared_box),
        # Nested Pages node: overrides MediaBox with an indirect ref.
        (5, b"<< /Type /Pages /Parent 2 0 R /Kids [6 0 R 7 0 R] /Count 2 "
            b"/MediaBox 4 0 R >>"),
        # Page 2: inherits the subtree's MediaBox (the shared 400pt box).
        (6, b"<< /Type /Page /Parent 5 0 R /Contents 8 0 R >>"),
        # Page 3: overrides MediaBox again, explicitly.
        (7, b"<< /Type /Page /Parent 5 0 R /MediaBox [0 0 200 100] >>"),
        # A non-stream /Contents: an empty array is legal (the page is blank).
        (8, b"[]"),
    ]
    write("pages.pdf", build_pdf(objects))


def incremental():
    """Two classic xref sections joined via /Prev (simulated update).

    The base section defines objects 1-5; the update redefines object 4
    (MediaBox of page 3 changes) and adds nothing else.
    """
    # --- base revision ---
    base = bytearray()
    base += b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox 4 0 R "
            b"/Resources << >> >>"),
        (4, b"[0 0 612 792]"),
        (5, b"<< /Producer (fixture-gen) >>"),
    ]
    for num, body in objs:
        offsets[num] = len(base)
        base += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    xref1 = len(base)
    base += b"xref\n0 6\n"
    base += b"0000000000 65535 f \n"
    for n in range(1, 6):
        base += f"{offsets[n]:010d} 00000 n \n".encode()
    base += b"trailer\n<< /Size 6 /Root 1 0 R /Info 5 0 R >>\n"
    # no startxref yet; appended by the update
    # --- update revision ---
    off4_new = len(base)
    base += b"4 0 obj\n[0 0 300 300]\nendobj\n"
    xref2 = len(base)
    base += b"xref\n4 1\n"
    base += f"{off4_new:010d} 00000 n \n".encode()
    base += b"trailer\n<< /Size 6 /Root 1 0 R /Prev " + str(xref1).encode() + b" >>\n"
    base += b"startxref\n" + str(xref2).encode() + b"\n%%EOF\n"
    write("incremental.pdf", bytes(base))


def xref_stream():
    """A syntactically valid xref-stream file (unsupported feature)."""
    out = bytearray()
    out += b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n"
    # Objects: catalog, pages, page — no classic xref.
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources << >> >>"),
    ]
    offsets = {}
    for num, body in objs:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    # Build the xref stream at object 4.
    W = [1, 4, 2]
    entry_len = sum(W)
    n_entries = 5  # objects 0..4
    data = bytearray()
    entries = [(0, 0, 255, 65535)]  # type 0 = free, f3=65535 gen
    for n in range(1, 5):
        if n in offsets:
            entries.append((n, 1, offsets[n], 0))
        else:
            entries.append((n, 0, 0, 65535))
    for (_num, t, f2, f3) in entries:
        data.append(t)
        data += f2.to_bytes(W[1], "big")
        data += f3.to_bytes(W[2], "big")
    xref_obj_pos = len(out)
    out += b"4 0 obj\n"
    out += (
        b"<< /Type /XRef /Length " + str(len(data)).encode()
        + b" /W [" + " ".join(str(w) for w in W).encode() + b"]"
        + b" /Size 5 /Root 1 0 R >>\nstream\n"
    )
    out += data + b"\nendstream\nendobj\n"
    out += b"startxref\n" + str(xref_obj_pos).encode() + b"\n%%EOF\n"
    write("xref_stream.pdf", bytes(out))
    _ = zlib  # imported for future compressed fixtures


def encrypted():
    """Classic xref table, but trailer declares /Encrypt."""
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
        (7, b"<< /Filter /Standard /V 1 /R 2 /O <00> /U <00> /P -4 >>"),
    ]
    write("encrypted.pdf", build_pdf(objects, trailer_extra=" /Encrypt 7 0 R /ID [<01><01>]"))


def bad_offset():
    """startxref deliberately points inside an object body."""
    data = bytearray(build_pdf([(1, b"<< /Type /Catalog /Pages 2 0 R >>")]))
    # Overwrite the numeric part of the last "startxref\n<digits>".
    idx = data.rfind(b"startxref")
    num_start = idx + len(b"startxref") + 1
    num_end = data.find(b"\n", num_start)
    data[num_start:num_end] = b"20"
    write("bad_offset.pdf", bytes(data))


def missing_ref():
    objects = [
        (1, b"<< /Type /Catalog /Pages 99 0 R >>"),
    ]
    write("missing_ref.pdf", build_pdf(objects))


def cyclic_pages():
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [2 0 R] /Count 1 /MediaBox [0 0 100 100] >>"),
    ]
    write("cyclic_pages.pdf", build_pdf(objects))


def truncated():
    full = build_pdf([
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ])
    write("truncated.pdf", full[: len(full) // 3])


# --------------------------------------------------------------------
# M1 fixtures
# --------------------------------------------------------------------


def flate_contents():
    """1 page whose /Contents is a FlateDecode'd stream."""
    raw = b"q 1 0 0 1 0 0 cm BT /F1 12 Tf (compressed page) Tj ET Q"
    comp = zlib.compress(raw)
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(comp)).encode()
            + b" /Filter /FlateDecode >>\nstream\n" + comp + b"\nendstream"),
    ]
    write("flate_contents.pdf", build_pdf(objects))


def contents_array():
    """1 page whose /Contents is an array of two streams."""
    a = b"q 1 0 0 1 0 0 cm"
    b_ = b"BT /F1 12 Tf (part two) Tj ET"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources << >> /Contents [4 0 R 5 0 R] >>"),
        (4, b"<< /Length " + str(len(a)).encode() + b" >>\nstream\n" + a + b"\nendstream"),
        (5, b"<< /Length " + str(len(b_)).encode() + b" /Filter /FlateDecode >>\nstream\n"
            + zlib.compress(b_) + b"\nendstream"),
    ]
    write("contents_array.pdf", build_pdf(objects))


def contents_none():
    """1 page with no /Contents at all."""
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ]
    write("contents_none.pdf", build_pdf(objects))


def flate_bad_data():
    """Truncated zlib data inside a FlateDecode stream."""
    raw = b"BT /F1 12 Tf (truncated) Tj ET"
    comp = zlib.compress(raw)[:8]  # cut mid-stream
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(comp)).encode()
            + b" /Filter /FlateDecode >>\nstream\n" + comp + b"\nendstream"),
    ]
    write("flate_bad_data.pdf", build_pdf(objects))


def flate_predictor():
    """/FlateDecode with /Predictor 15 (PNG optimum) on a small image."""
    # 3x1 RGB image, each row prefixed by filter byte 0.
    row = b"\x00" + b"\x11\x22\x33" * 3  # filter 0 + 9 bytes
    comp = zlib.compress(row)
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(comp)).encode()
            + b" /Filter /FlateDecode /DecodeParms << /Predictor 15 /Colors 3 /BitsPerComponent 8 /Columns 3 >> >>\nstream\n"
            + comp + b"\nendstream"),
    ]
    write("flate_predictor.pdf", build_pdf(objects))


def flate_predictor_bad():
    """/Predictor 16 is out of range -> Unsupported."""
    row = b"\x00" + b"\x11\x22\x33" * 3
    comp = zlib.compress(row)
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(comp)).encode()
            + b" /Filter /FlateDecode /DecodeParms << /Predictor 16 >> >>\nstream\n"
            + comp + b"\nendstream"),
    ]
    write("flate_predictor_bad.pdf", build_pdf(objects))


def unsupported_filter():
    """Stream with a filter we don't implement (/JPXDecode)."""
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Contents 4 0 R >>"),
        (4, b"<< /Length 10 /Filter /JPXDecode >>\nstream\n" + b"\x00" * 10 + b"\nendstream"),
    ]
    write("unsupported_filter.pdf", build_pdf(objects))


def objstm():
    """Page tree where the page object is inside an /ObjStm (object 5).
    Object 4 is the ObjStm; the page (5) is compressed inside it.
    Object 6 is the page's /Contents stream (regular object).
    The xref stream is used so entry types 1 and 2 can coexist."""
    page_dict = (b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] "
                 b"/Resources << >> /Contents 6 0 R >>")
    header = b"5 0"  # objnum 5 at offset 0
    body = page_dict
    first = len(header) + 1  # header + one space separator
    data = header + b" " + body
    comp = zlib.compress(data)
    contents_stream = b"q 1 0 0 1 10 10 cm Q"

    out = bytearray()
    out += b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 /MediaBox [0 0 612 792] >>"),
        (4, b"<< /Type /ObjStm /N 1 /First " + str(first).encode()
            + b" /Length " + str(len(comp)).encode()
            + b" /Filter /FlateDecode >>\nstream\n" + comp + b"\nendstream"),
        (6, b"<< /Length " + str(len(contents_stream)).encode() + b" >>\nstream\n"
            + contents_stream + b"\nendstream"),
    ]
    for num, body in objs:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    # xref stream at object 7
    xref_obj_pos = len(out)
    W = [1, 4, 2]
    entry_len = sum(W)
    # Entries for 0..7
    entries = [
        (0, 0, 0, 65535),                  # free
        (1, 1, offsets[1], 0),
        (2, 1, offsets[2], 0),
        (3, 0, 0, 0),                      # obj 3 unused
        (4, 1, offsets[4], 0),             # ObjStm
        (5, 2, 4, 0),                      # page inside ObjStm 4, index 0
        (6, 1, offsets[6], 0),             # contents
        (7, 1, xref_obj_pos, 0),           # xref stream itself
    ]
    data2 = bytearray()
    for (_num, t, f2, f3) in entries:
        data2.append(t)
        data2 += f2.to_bytes(W[1], "big")
        data2 += f3.to_bytes(W[2], "big")
    out += b"7 0 obj\n"
    out += (
        b"<< /Type /XRef /Length " + str(len(data2)).encode()
        + b" /W [" + " ".join(str(w) for w in W).encode() + b"]"
        + b" /Size 8 /Root 1 0 R >>\nstream\n"
    )
    out += data2 + b"\nendstream\nendobj\n"
    out += b"startxref\n" + str(xref_obj_pos).encode() + b"\n%%EOF\n"
    write("objstm.pdf", bytes(out))


def hybrid():
    """Hybrid-reference file: classic xref + /XRefStm stream.
    Both name the same objects (1,2,3 in-use); the stream additionally
    marks object 3 free — per §7.5.8.4 the stream wins in the same
    update section, so resolving object 3 must fail."""
    out = bytearray()
    out += b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 500 500] >>"),
    ]
    for num, body in objs:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    # Classic xref at offset X.
    xref_pos = len(out)
    out += b"xref\n0 4\n"
    out += b"0000000000 65535 f \n"
    for n in range(1, 4):
        out += f"{offsets[n]:010d} 00000 n \n".encode()
    # The classic trailer says /XRefStm points at object 4.  The xref
    # stream itself is written next, so its offset is the current length
    # *after* the trailer placeholder is expanded to its final width.
    trailer_placeholder = b"trailer\n<< /Size 5 /Root 1 0 R /XRefStm     >>\n"
    out += trailer_placeholder
    xrefstm_obj_pos = len(out)
    # Patch the trailer with the real offset (same byte length).
    trailer_real = trailer_placeholder.replace(
        b"    ", str(xrefstm_obj_pos).encode().rjust(5, b" ")
    )
    out = bytearray(out[: len(out) - len(trailer_placeholder)]) + trailer_real
    W = [1, 4, 2]
    entry_len = sum(W)
    data = bytearray()
    # Stream entries: objects 0,1,2 agree with the table; object 3 is
    # marked free (overriding the table's in-use entry), object 4 is the
    # stream itself.
    entries = [
        (0, 0, 0, 65535),
        (1, 1, offsets[1], 0),
        (2, 1, offsets[2], 0),
        (3, 0, 0, 0),                  # free: overrides classic InUse
        (4, 1, xrefstm_obj_pos, 0),
    ]
    for (_num, t, f2, f3) in entries:
        data.append(t)
        data += f2.to_bytes(W[1], "big")
        data += f3.to_bytes(W[2], "big")
    out += b"4 0 obj\n"
    out += (
        b"<< /Type /XRef /Length " + str(len(data)).encode()
        + b" /W [" + " ".join(str(w) for w in W).encode() + b"]"
        + b" /Size 5 /Root 1 0 R >>\nstream\n"
    )
    out += data + b"\nendstream\nendobj\n"
    out += b"startxref\n" + str(xref_pos).encode() + b"\n%%EOF\n"
    write("hybrid.pdf", bytes(out))


def hybrid_compressed():
    """Hybrid file where the classic table marks object 5 free and the
    /XRefStm stream marks it compressed inside object stream 4."""
    page_dict = (b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] "
                 b"/Resources << >> >>")
    header = b"5 0"
    first = len(header) + 1
    data = header + b" " + page_dict
    comp = zlib.compress(data)

    out = bytearray()
    out += b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>"),
        (4, b"<< /Type /ObjStm /N 1 /First " + str(first).encode()
            + b" /Length " + str(len(comp)).encode()
            + b" /Filter /FlateDecode >>\nstream\n" + comp + b"\nendstream"),
    ]
    for num, body in objs:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    xref_pos = len(out)
    out += b"xref\n0 5\n"  # classic: 0..4
    out += b"0000000000 65535 f \n"
    for n in range(1, 5):
        if n in offsets:
            out += f"{offsets[n]:010d} 00000 n \n".encode()
        else:
            out += b"0000000000 00000 f \n"
    trailer_placeholder = b"trailer\n<< /Size 7 /Root 1 0 R /XRefStm     >>\n"
    out += trailer_placeholder
    xrefstm_obj_pos = len(out)
    trailer_real = trailer_placeholder.replace(
        b"    ", str(xrefstm_obj_pos).encode().rjust(5, b" ")
    )
    out = bytearray(out[: len(out) - len(trailer_placeholder)]) + trailer_real
    W = [1, 4, 2]
    data2 = bytearray()
    entries = [
        (0, 0, 0, 65535),
        (1, 1, offsets[1], 0),
        (2, 1, offsets[2], 0),
        (3, 0, 0, 0),
        (4, 1, offsets[4], 0),        # the ObjStm itself
        (5, 2, 4, 0),                 # compressed in ObjStm 4, index 0
        (6, 1, xrefstm_obj_pos, 0),   # the xref stream
    ]
    for (_num, t, f2, f3) in entries:
        data2.append(t)
        data2 += f2.to_bytes(W[1], "big")
        data2 += f3.to_bytes(W[2], "big")
    out += b"6 0 obj\n"
    out += (
        b"<< /Type /XRef /Length " + str(len(data2)).encode()
        + b" /W [" + " ".join(str(w) for w in W).encode() + b"]"
        + b" /Size 7 /Root 1 0 R >>\nstream\n"
    )
    out += data2 + b"\nendstream\nendobj\n"
    out += b"startxref\n" + str(xref_pos).encode() + b"\n%%EOF\n"
    write("hybrid_compressed.pdf", bytes(out))


def hybrid_gap():
    """Hybrid file where the classic table marks object 5 free but the
    /XRefStm stream supplies it as in-use -> the stream wins."""
    out = bytearray()
    out += b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [5 0 R] /Count 1 >>"),
        (5, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] "
            b"/Resources << >> >>"),
    ]
    for num, body in objs:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    xref_pos = len(out)
    out += b"xref\n0 4\n"  # classic table knows only 0..3
    out += b"0000000000 65535 f \n"
    for n in range(1, 4):
        if n in offsets:
            out += f"{offsets[n]:010d} 00000 n \n".encode()
        else:
            out += b"0000000000 00000 f \n"
    trailer_placeholder = b"trailer\n<< /Size 6 /Root 1 0 R /XRefStm     >>\n"
    out += trailer_placeholder
    xrefstm_obj_pos = len(out)
    trailer_real = trailer_placeholder.replace(
        b"    ", str(xrefstm_obj_pos).encode().rjust(5, b" ")
    )
    out = bytearray(out[: len(out) - len(trailer_placeholder)]) + trailer_real
    W = [1, 4, 2]
    data = bytearray()
    # Stream covers objects 0..5; object 5 is in-use in the stream
    # (filling the gap left by the classic table).
    entries = [
        (0, 0, 0, 65535),
        (1, 1, offsets[1], 0),
        (2, 1, offsets[2], 0),
        (3, 0, 0, 0),
        (4, 1, xrefstm_obj_pos, 0),
        (5, 1, offsets[5], 0),
    ]
    for (_num, t, f2, f3) in entries:
        data.append(t)
        data += f2.to_bytes(W[1], "big")
        data += f3.to_bytes(W[2], "big")
    out += b"4 0 obj\n"
    out += (
        b"<< /Type /XRef /Length " + str(len(data)).encode()
        + b" /W [" + " ".join(str(w) for w in W).encode() + b"]"
        + b" /Size 6 /Root 1 0 R >>\nstream\n"
    )
    out += data + b"\nendstream\nendobj\n"
    out += b"startxref\n" + str(xref_pos).encode() + b"\n%%EOF\n"
    write("hybrid_gap.pdf", bytes(out))


def prev_chain():
    """Two classic sections via /Prev; newer overrides an object."""
    out = bytearray()
    out += b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 500 500] >>"),
    ]
    for num, body in objs:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    xref1 = len(out)
    out += b"xref\n0 4\n"
    out += b"0000000000 65535 f \n"
    for n in range(1, 4):
        out += f"{offsets[n]:010d} 00000 n \n".encode()
    out += b"trailer\n<< /Size 4 /Root 1 0 R >>\n"
    # Update section redefines obj 3 with a different MediaBox.
    off3 = len(out)
    out += b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 999 999] >>\nendobj\n"
    xref2 = len(out)
    out += b"xref\n3 1\n"
    out += f"{off3:010d} 00000 n \n".encode()
    out += b"trailer\n<< /Size 4 /Root 1 0 R /Prev " + str(xref1).encode() + b" >>\n"
    out += b"startxref\n" + str(xref2).encode() + b"\n%%EOF\n"
    write("prev_chain.pdf", bytes(out))


def xref_stream_bad_type():
    """xref stream with an invalid entry type (9)."""
    out = bytearray()
    out += b"%PDF-1.5\n%\xe2\xe3\xcf\xd3\n"
    offsets = {}
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
    ]
    for num, body in objs:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    xref_obj_pos = len(out)
    W = [1, 4, 2]
    data = bytearray()
    entries = [
        (0, 0, 0, 65535),
        (1, 1, offsets[1], 0),
        (2, 1, offsets[2], 0),
        (3, 9, offsets[3], 0),  # bad type 9
        (4, 1, xref_obj_pos, 0),
    ]
    for (_num, t, f2, f3) in entries:
        data.append(t)
        data += f2.to_bytes(W[1], "big")
        data += f3.to_bytes(W[2], "big")
    out += b"4 0 obj\n"
    out += (
        b"<< /Type /XRef /Length " + str(len(data)).encode()
        + b" /W [" + " ".join(str(w) for w in W).encode() + b"]"
        + b" /Size 5 /Root 1 0 R >>\nstream\n"
    )
    out += data + b"\nendstream\nendobj\n"
    out += b"startxref\n" + str(xref_obj_pos).encode() + b"\n%%EOF\n"
    write("xref_stream_bad_type.pdf", bytes(out))


# --------------------------------------------------------------------
# M2 fixtures
# --------------------------------------------------------------------


def content_ops():
    """Content stream covering the operators M2 parses, incl. a
    compatibility section, marked content, XObject/ExtGState names and
    a TJ array with kerning numbers."""
    stream = (
        b"q\n"
        b"2 w 1 J 0 j 10 M [3 2] 0 d /RelativeColorimetric ri 1 i /GS1 gs\n"
        b"1 0 0 1 50 60 cm\n"
        b"10 20 m 30 40 l 1 2 3 4 5 6 c 7 8 9 10 v 11 12 13 14 y h\n"
        b"100 100 50 30 re\n"
        b"S s f F f* B B* b b* n W W*\n"
        b"0.5 G 0.2 g 0.1 0.2 0.3 RG 0.4 0.5 0.6 rg\n"
        b"0 1 0 0 K 0 0 1 0 k\n"
        b"/CS1 CS /CS2 cs 0.5 0.5 SC 0.1 0.2 sc\n"
        b"BT /F1 12 Tf 0.5 Tc 1.5 Tw 110 Tz 14 TL 2 Tr 3 Ts\n"
        b"10 20 Td 1 2 TD 1 0 0 1 30 40 Tm T*\n"
        b"(abc) Tj [(a) -250 (b) 30] TJ (c) ' 0.5 0.25 (d) \"\n"
        b"ET\n"
        b"/Im1 Do /Sh1 sh\n"
        b"/Tag MP /Tag << /K 1 >> DP\n"
        b"/Tag BMC /Tag2 << /K (v) >> BDC EMC EMC\n"
        b"BX /Foo 1 2 Bar EX\n"
        b"Q"
    )
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Resources << /Font << /F1 5 0 R >> "
            b"/XObject << /Im1 6 0 R >> /ExtGState << /GS1 7 0 R >> "
            b"/ColorSpace << /CS1 8 0 R >> /Shading << /Sh1 9 0 R >> >> "
            b"/Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
        (6, b"<< /Subtype /Form /BBox [0 0 10 10] /Length 0 >>\nstream\n\nendstream"),
        (7, b"<< /Type /ExtGState /CA 0.5 >>"),
        (8, b"/DeviceRGB"),
        (9, b"<< /ShadingType 2 /ColorSpace /DeviceRGB >>"),
    ]
    write("content_ops.pdf", build_pdf(objects))


# --------------------------------------------------------------------
# M3 fixtures
# --------------------------------------------------------------------


def render_rects():
    """A page using only supported drawing ops: two overlapping
    rectangles (fill + stroke), a clipping rect, a cm transform, q/Q
    colour restore, DeviceGray/RGB/CMYK colours, dash."""
    stream = (
        # Page 200x200pt. White bg.
        b"q\n"
        b"0 0 0.8 RG\n"                       # stroke blue-ish
        b"0.5 g\n"                            # fill mid-gray
        b"20 20 60 60 re B\n"                 # fill+stroke rect
        b"Q\n"
        # After Q colours revert to black.
        b"2 w 1 0 0 RG 0 1 0 rg 60 60 80 40 re B\n"
        # cm transform: rotate 45° about (100,100) via translate+rotate.
        b"q 1 0 0 1 100 100 cm 0.7071 0.7071 -0.7071 0.7071 0 0 cm\n"
        b"1 0 0 0 k\n"                        # cyan fill
        b"-10 -10 20 20 re f\n"
        b"Q\n"
        # Clipping: clip to right half, fill should be clipped.
        b"100 0 100 200 re W n\n"
        b"1 0 0 rg 0 0 200 200 re f\n"
    )
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] "
            b"/Resources << >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    write("render_rects.pdf", build_pdf(objects))


def render_annot():
    """Page carrying an /Annots array — render must refuse."""
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Annots [5 0 R] /Contents 4 0 R >>"),
        (4, b"<< /Length 10 >>\nstream\n" + b"0 0 10 10 re" + b"\nendstream"),
        (5, b"<< /Subtype /Text /Rect [0 0 10 10] /Contents (note) >>"),
    ]
    write("render_annot.pdf", build_pdf(objects))


def render_text():
    """Page with a Tj — render must refuse (text is out of M3)."""
    stream = b"BT /F1 12 Tf 10 50 Td (hi) Tj ET"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"),
    ]
    write("render_text.pdf", build_pdf(objects))


# ---------------------------------------------------------------------
# M4: Form XObjects and images
# ---------------------------------------------------------------------

def xobject_form_basic():
    """Form XObject with identity Matrix, no own Resources, painting a
    50x50 red square. Page paints it at (10,10) via cm."""
    form_stream = b"1 0 0 rg 0 0 50 50 re f"
    page_stream = b"q 1 0 0 1 10 10 cm /Fm1 Do Q"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Fm1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Form /BBox [0 0 50 50] "
            b"/Length " + str(len(form_stream)).encode() + b" >>\nstream\n"
            + form_stream + b"\nendstream"),
    ]
    write("xobject_form_basic.pdf", build_pdf(objects))


def xobject_form_matrix():
    """Form with a translation Matrix: painted content is shifted."""
    form_stream = b"0 0 1 rg 0 0 10 10 re f"
    page_stream = b"/Fm2 Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Fm2 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] "
            b"/Matrix [1 0 0 1 40 40] "
            b"/Length " + str(len(form_stream)).encode() + b" >>\nstream\n"
            + form_stream + b"\nendstream"),
    ]
    write("xobject_form_matrix.pdf", build_pdf(objects))


def xobject_form_bbox_clip():
    """Form's BBox clips its content: a 20x20 form drawn where the page
    expects only the BBox to show."""
    form_stream = b"1 0 0 rg 0 0 100 100 re f"  # bigger than BBox
    page_stream = b"/Fm3 Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Fm3 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Form /BBox [10 10 30 30] "
            b"/Length " + str(len(form_stream)).encode() + b" >>\nstream\n"
            + form_stream + b"\nendstream"),
    ]
    write("xobject_form_bbox_clip.pdf", build_pdf(objects))


def xobject_form_own_resources():
    """Form with its own /Resources defining a color-op XObject; the
    page's resources must NOT leak into the form."""
    # Form fills with whatever /CS its own Resources define — we keep it
    # simple: form uses rg directly but declares an empty Resources dict
    # to prove replacement.
    form_stream = b"0 1 0 rg 5 5 15 15 re f"
    page_stream = b"/Fm4 Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Fm4 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Form /BBox [0 0 25 25] "
            b"/Resources << >> "
            b"/Length " + str(len(form_stream)).encode() + b" >>\nstream\n"
            + form_stream + b"\nendstream"),
    ]
    write("xobject_form_own_resources.pdf", build_pdf(objects))


def xobject_form_nested():
    """Form A invokes form B (nested Do)."""
    inner_stream = b"0 0 1 rg 0 0 10 10 re f"
    outer_stream = b"q 1 0 0 1 20 20 cm /FmIn Do Q"
    page_stream = b"/FmOut Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /FmOut 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Form /BBox [0 0 40 40] "
            b"/Resources << /XObject << /FmIn 6 0 R >> >> "
            b"/Length " + str(len(outer_stream)).encode() + b" >>\nstream\n"
            + outer_stream + b"\nendstream"),
        (6, b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] "
            b"/Length " + str(len(inner_stream)).encode() + b" >>\nstream\n"
            + inner_stream + b"\nendstream"),
    ]
    write("xobject_form_nested.pdf", build_pdf(objects))


def xobject_form_self_recursive():
    """Form that invokes itself — must be rejected (cycle)."""
    form_stream = b"/FmSelf Do"
    page_stream = b"/FmSelf Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /FmSelf 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] "
            b"/Resources << /XObject << /FmSelf 5 0 R >> >> "
            b"/Length " + str(len(form_stream)).encode() + b" >>\nstream\n"
            + form_stream + b"\nendstream"),
    ]
    write("xobject_form_self_recursive.pdf", build_pdf(objects))


def xobject_form_group():
    """Form with /Group (transparency group) — must be refused."""
    form_stream = b"0 0 0 rg 0 0 10 10 re f"
    page_stream = b"/FmG Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /FmG 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Form /BBox [0 0 10 10] "
            b"/Group << /S /Transparency /CS /DeviceRGB >> "
            b"/Length " + str(len(form_stream)).encode() + b" >>\nstream\n"
            + form_stream + b"\nendstream"),
    ]
    write("xobject_form_group.pdf", build_pdf(objects))


def xobject_image_gray_raw():
    """4x4 DeviceGray image, raw bytes, placed 10..50 via cm."""
    # 4x4 grayscale: gradient 0x00,0x40,0x80,0xFF per row
    img = bytes(range(0, 256, 16))[:16]
    page_stream = b"q 40 0 0 40 10 10 cm /Im1 Do Q"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Im1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 "
            b"/BitsPerComponent 8 /ColorSpace /DeviceGray "
            b"/Length " + str(len(img)).encode() + b" >>\nstream\n"
            + img + b"\nendstream"),
    ]
    write("xobject_image_gray_raw.pdf", build_pdf(objects))


def xobject_image_rgb_flate():
    """4x4 DeviceRGB image, FlateDecode."""
    img = bytes([
        255,0,0,   0,255,0,   0,0,255,   255,255,0,   # row 0
        0,255,255, 255,0,255, 128,128,128, 0,0,0,     # row 1
        255,255,255, 64,64,64, 192,192,192, 32,32,32, # row 2
        255,128,0,   0,128,255, 128,0,128, 255,0,0,   # row 3
    ])
    enc = zlib.compress(img)
    page_stream = b"q 30 0 0 30 20 20 cm /Im2 Do Q"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Im2 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Image /Width 4 /Height 4 "
            b"/BitsPerComponent 8 /ColorSpace /DeviceRGB "
            b"/Filter /FlateDecode "
            b"/Length " + str(len(enc)).encode() + b" >>\nstream\n"
            + enc + b"\nendstream"),
    ]
    write("xobject_image_rgb_flate.pdf", build_pdf(objects))


def xobject_image_decode_invert():
    """Image with /Decode [1 0] — inverts gray."""
    img = bytes([0, 128, 255, 64])
    page_stream = b"q 20 0 0 20 10 10 cm /Im3 Do Q"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Im3 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Image /Width 2 /Height 2 "
            b"/BitsPerComponent 8 /ColorSpace /DeviceGray "
            b"/Decode [1 0] "
            b"/Length " + str(len(img)).encode() + b" >>\nstream\n"
            + img + b"\nendstream"),
    ]
    write("xobject_image_decode_invert.pdf", build_pdf(objects))


def xobject_image_mask():
    """Image with /ImageMask true — must be refused."""
    img = bytes([0xFF])
    page_stream = b"/ImM Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /ImM 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Image /Width 8 /Height 1 "
            b"/BitsPerComponent 1 /ImageMask true "
            b"/Length " + str(len(img)).encode() + b" >>\nstream\n"
            + img + b"\nendstream"),
    ]
    write("xobject_image_mask.pdf", build_pdf(objects))


def xobject_image_smask():
    """Image with /SMask — must be refused."""
    img = bytes([255, 0, 0, 0, 255, 0])
    page_stream = b"/ImS Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /ImS 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Image /Width 2 /Height 1 "
            b"/BitsPerComponent 8 /ColorSpace /DeviceRGB "
            b"/SMask 6 0 R "
            b"/Length " + str(len(img)).encode() + b" >>\nstream\n"
            + img + b"\nendstream"),
        (6, b"<< /Type /XObject /Subtype /Image /Width 2 /Height 1 "
            b"/BitsPerComponent 8 /ColorSpace /DeviceGray "
            b"/Length 2 >>\nstream\n\xff\xff\nendstream"),
    ]
    write("xobject_image_smask.pdf", build_pdf(objects))


def xobject_image_dct():
    """Image XObject with DCTDecode — real JPEG bytes generated via PIL."""
    try:
        from PIL import Image
        import io
        # 8x8 RGB JPEG, deterministic gradient
        img = Image.new("RGB", (8, 8))
        px = img.load()
        for y in range(8):
            for x in range(8):
                px[x, y] = (x * 32, y * 32, 128)
        buf = io.BytesIO()
        img.save(buf, format="JPEG", quality=90)
        enc = buf.getvalue()
    except ImportError:
        # Fallback: embed a pre-made minimal baseline JPEG.
        # A 1x1 black JPEG (JFIF), ~160 bytes, well-formed.
        enc = bytes.fromhex(
            "ffd8ffe000104a46494600010100000100010000"
            "ffdb004300" + "08" * 64 +
            "ffc0000b080001000101011100"
            "ffc40014000100000000000000000000000000000000000000"
            "ffc40014010100000000000000000000000000000000000000"
            "ffda0008010100003f00d2cf20ffd9"
        )
    page_stream = b"q 40 0 0 40 10 10 cm /ImJ Do Q"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /ImJ 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 "
            b"/BitsPerComponent 8 /ColorSpace /DeviceRGB "
            b"/Filter /DCTDecode "
            b"/Length " + str(len(enc)).encode() + b" >>\nstream\n"
            + enc + b"\nendstream"),
    ]
    write("xobject_image_dct.pdf", build_pdf(objects))


def xobject_unknown_subtype():
    """XObject with /Subtype /PS — must be refused."""
    stream = b"%!PS\n"
    page_stream = b"/PS1 Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /PS1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /PS "
            b"/Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    write("xobject_unknown_subtype.pdf", build_pdf(objects))


def xobject_missing():
    """Do referencing an undefined XObject name — Structure error."""
    page_stream = b"/NoSuch Do"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
    ]
    write("xobject_missing.pdf", build_pdf(objects))


def xobject_unused_bad():
    """Page has an unsupported /Subtype /PS XObject in resources but
    never invokes it — page must render fine (unused is OK)."""
    stream = b"0 0 1 rg 0 0 10 10 re f"
    ps_stream = b"%!PS junk\n"
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] "
            b"/Resources << /XObject << /Unused 5 0 R >> >> "
            b"/Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /PS "
            b"/Length " + str(len(ps_stream)).encode() + b" >>\nstream\n"
            + ps_stream + b"\nendstream"),
    ]
    write("xobject_unused_bad.pdf", build_pdf(objects))


# ---------------------------------------------------------------------
# M5: embedded TrueType text
# ---------------------------------------------------------------------

def _font_objects(ttf_bytes, widths):
    """Return (font dict, descriptor, file2) objects for the TTF.
    `widths` is a list of (char_code, width) covering FirstChar..LastChar."""
    return ttf_bytes, widths


def font_ttf():
    """Load the test TTF produced by make_font.py."""
    with open(os.path.join(HERE, "KasaneTest.ttf"), "rb") as f:
        return f.read()


def _mk_font(objs, num_font, num_fd, num_ff, ttf_bytes,
             first=32, last=255, widths=None, encoding=b"/WinAnsiEncoding",
             flags=32):
    """Append a font dict + descriptor + flate-encoded FontFile2."""
    if widths is None:
        widths = [600] * (last - first + 1)
        widths[0] = 250  # space
    warr = b"[" + b" ".join(str(w).encode() for w in widths) + b"]"
    enc = zlib.compress(ttf_bytes)
    objs.append((num_ff,
        b"<< /Length " + str(len(enc)).encode()
        + b" /Filter /FlateDecode /Length1 " + str(len(ttf_bytes)).encode()
        + b" >>\nstream\n" + enc + b"\nendstream"))
    objs.append((num_fd,
        b"<< /Type /FontDescriptor /FontName /KasaneTest-Regular "
        b"/Flags " + str(flags).encode()
        + b" /FontBBox [0 -200 600 800] /ItalicAngle 0 "
        b"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 "
        b"/FontFile2 " + str(num_ff).encode() + b" 0 R >>"))
    objs.append((num_font,
        b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular "
        b"/Encoding " + encoding
        + b" /FirstChar " + str(first).encode()
        + b" /LastChar " + str(last).encode()
        + b" /Widths " + warr
        + b" /FontDescriptor " + str(num_fd).encode() + b" 0 R >>"))


def text_basic():
    """BT /F1 24 Tf 20 60 Td (ABC) Tj ET — three A/B/C glyphs."""
    stream = b"BT /F1 24 Tf 20 60 Td (ABC) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_basic.pdf", build_pdf(objs))


def text_tj():
    """TJ with positioning adjustments."""
    stream = b"BT /F1 20 Tf 10 50 Td [(A) -120 (B) 60 (C)] TJ ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_tj.pdf", build_pdf(objs))


def text_quote_ops():
    """' and \" operators — both take the string as their operand."""
    stream = (b"BT /F1 20 Tf 14 TL 10 80 Td (First) Tj "
              b"(Second) ' 5 5 (Third) \" ET")
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_quote_ops.pdf", build_pdf(objs))


def text_tc_tw_tz_ts():
    """Tc, Tw, Tz, Ts applied."""
    stream = (b"BT /F1 20 Tf 10 50 Td "
              b"2 Tc 5 Tw (A B) Tj "
              b"-2 Tc -5 Tw "
              b"50 Tz 0 -20 Td (XY) Tj "
              b"100 Tz 5 Ts 0 -20 Td (Z) Tj ET")
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_tc_tw_tz_ts.pdf", build_pdf(objs))


def text_render_modes():
    """Tr 0/1/2/3 — fill, stroke, fill+stroke, invisible."""
    stream = (b"BT /F1 20 Tf 10 80 Td 0 Tr (F) Tj ET "
              b"BT /F1 20 Tf 10 60 Td 1 Tr (S) Tj ET "
              b"BT /F1 20 Tf 10 40 Td 2 Tr (B) Tj ET "
              b"BT /F1 20 Tf 10 20 Td 3 Tr (I) Tj ET")
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_render_modes.pdf", build_pdf(objs))


def text_mode4_refused():
    """Tr 4 — fill + clip mode (§9.3.6); renders since text clipping landed (name kept for the fixture)."""
    stream = b"BT /F1 20 Tf 10 50 Td 4 Tr (X) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_mode4_refused.pdf", build_pdf(objs))


def text_in_form():
    """Form containing text — the form's own /Font must be used."""
    form_stream = b"BT /F2 20 Tf 0 0 Td (XY) Tj ET"
    page_stream = b"/FmT Do"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /XObject << /FmT 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
        (5, b"<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] "
            b"/Resources << /Font << /F2 6 0 R >> >> "
            b"/Matrix [1 0 0 1 50 20] "
            b"/Length " + str(len(form_stream)).encode() + b" >>\nstream\n"
            + form_stream + b"\nendstream"),
    ]
    _mk_font(objs, 6, 7, 8, font_ttf())
    write("text_in_form.pdf", build_pdf(objs))


def text_same_name_two_resources():
    """Page and Form each define /F1 — the form's must win inside
    the form; the page's must be used outside."""
    page_stream = b"BT /F1 20 Tf 10 80 Td (Pg) Tj ET /FmN Do"
    form_stream = b"BT /F1 20 Tf 0 0 Td (Fm) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> "
            b"/XObject << /FmN 8 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(page_stream)).encode() + b" >>\nstream\n"
            + page_stream + b"\nendstream"),
    ]
    # Page's F1: first char widths different so we can tell apart
    _mk_font(objs, 5, 6, 7, font_ttf())
    # Form's F1 — same name, different dict (e.g. different FirstChar)
    objs.append((8,
        b"<< /Type /XObject /Subtype /Form /BBox [0 0 100 100] "
        b"/Resources << /Font << /F1 9 0 R >> >> "
        b"/Matrix [1 0 0 1 100 10] "
        b"/Length " + str(len(form_stream)).encode() + b" >>\nstream\n"
        + form_stream + b"\nendstream"))
    _mk_font(objs, 9, 10, 11, font_ttf())
    write("text_same_name_two_resources.pdf", build_pdf(objs))


def text_unsupported_type1():
    """Font with /Subtype /Type1 — refused."""
    stream = b"BT /F1 20 Tf 10 50 Td (X) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
        (5, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica "
            b"/Encoding /WinAnsiEncoding >>"),
    ]
    write("text_unsupported_type1.pdf", build_pdf(objs))


def text_differences_encoding():
    """Encoding dictionary with Differences over WinAnsi."""
    stream = b"BT /F1 24 Tf 20 60 Td (AB) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    # Encoding dict: BaseEncoding WinAnsi, code 65 → /B (swap A and B)
    enc = b"<< /Type /Encoding /BaseEncoding /WinAnsiEncoding " \
          b"/Differences [65 /B 66 /A] >>"
    objs.append((9, enc))
    # Manually construct font so we can reference the encoding dict
    first, last = 32, 255
    widths = [600] * (last - first + 1)
    widths[0] = 250
    warr = b"[" + b" ".join(str(w).encode() for w in widths) + b"]"
    ttf = font_ttf()
    enc_stream = zlib.compress(ttf)
    objs.append((7,
        b"<< /Length " + str(len(enc_stream)).encode()
        + b" /Filter /FlateDecode /Length1 " + str(len(ttf)).encode()
        + b" >>\nstream\n" + enc_stream + b"\nendstream"))
    objs.append((6,
        b"<< /Type /FontDescriptor /FontName /KasaneTest-Regular "
        b"/Flags 32 /FontBBox [0 -200 600 800] /ItalicAngle 0 "
        b"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 "
        b"/FontFile2 7 0 R >>"))
    objs.append((5,
        b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular "
        b"/Encoding 9 0 R "
        b"/FirstChar " + str(first).encode()
        + b" /LastChar " + str(last).encode()
        + b" /Widths " + warr
        + b" /FontDescriptor 6 0 R >>"))
    write("text_differences_encoding.pdf", build_pdf(objs))


def text_notdef():
    """Char code with no encoding entry → .notdef glyph.
    0x01 is embedded as an octal escape — \\001 in the string."""
    stream = b"BT /F1 24 Tf 20 60 Td (A\\001Z) Tj ET"  # \\001 = char 0x01
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_notdef.pdf", build_pdf(objs))


def text_unsupported_encoding():
    """Encoding /MacRomanEncoding — refused."""
    stream = b"BT /F1 24 Tf 20 60 Td (A) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf(), encoding=b"/MacRomanEncoding")
    write("text_unsupported_encoding.pdf", build_pdf(objs))


def text_symbolic_refused():
    """Symbolic flag set — refused."""
    stream = b"BT /F1 24 Tf 20 60 Td (A) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf(), flags=4)  # symbolic
    write("text_symbolic_refused.pdf", build_pdf(objs))


def text_composite_glyph():
    """WinAnsi 0xC4 = Ä — exercises the composite glyph."""
    stream = b"BT /F1 24 Tf 20 60 Td (\\xC4) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_composite_glyph.pdf", build_pdf(objs))


def text_rotated():
    """cm-rotated text — Tm composes with CTM."""
    import math
    c, s = math.cos(math.radians(90)), math.sin(math.radians(90))
    stream = (b"q " + f"{c:.4f} {s:.4f} {-s:.4f} {c:.4f} 20 60 cm".encode()
              + b" BT /F1 20 Tf (Hi) Tj ET Q")
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_rotated.pdf", build_pdf(objs))


def text_scaled():
    """Non-uniform cm scale on text."""
    stream = b"q 2 0 0 0.5 10 50 cm BT /F1 20 Tf (Hi) Tj ET Q"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_scaled.pdf", build_pdf(objs))


def text_clip():
    """Clipped text — only part of the glyph shows."""
    stream = (b"q 0 0 30 30 re W n "
              b"BT /F1 24 Tf 10 40 Td (Hi) Tj ET Q")
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_clip.pdf", build_pdf(objs))


def text_extended_chars():
    """WinAnsi 0x80+ range: Euro, quotes, accents — exercises the
    encoding table's upper half."""
    stream = (b"BT /F1 20 Tf 10 80 Td "
              b"(\\x80\\x93\\x94\\xA9\\xC4\\xE9) Tj ET")
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_extended_chars.pdf", build_pdf(objs))


def text_tj_split():
    """TJ splitting 'AB' into separate strings vs. plain 'AB' — same
    output expected."""
    stream = (b"BT /F1 20 Tf 10 80 Td [(A) (B)] TJ ET "
              b"BT /F1 20 Tf 10 50 Td (AB) Tj ET")
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_tj_split.pdf", build_pdf(objs))


def text_empty_string():
    """() Tj — empty string advances nothing, draws nothing."""
    stream = b"BT /F1 20 Tf 10 80 Td () Tj (A) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_empty_string.pdf", build_pdf(objs))


def text_tj_consecutive_numbers():
    """TJ with consecutive numbers — no strings between them."""
    stream = b"BT /F1 20 Tf 10 80 Td [(A) -50 -50 (B)] TJ ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_tj_consecutive_numbers.pdf", build_pdf(objs))


def text_missing_font_ref():
    """Tf references a name not in /Font — Structure error."""
    stream = b"BT /F9 20 Tf 10 80 Td (A) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_missing_font_ref.pdf", build_pdf(objs))


def text_no_font_tj():
    """Tj before any Tf — Structure error."""
    stream = b"BT 10 80 Td (A) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    _mk_font(objs, 5, 6, 7, font_ttf())
    write("text_no_font_tj.pdf", build_pdf(objs))


def text_widths_override():
    stream = b"BT /F1 20 Tf 10 60 Td (AB) Tj ET"
    objs = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
            b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>"),
        (4, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
            + stream + b"\nendstream"),
    ]
    # Widths: A=1000 (wider than font's 600), B=200
    first, last = 65, 66
    widths = [1000, 200]
    warr = b"[1000 200]"
    ttf = font_ttf()
    enc_stream = zlib.compress(ttf)
    objs.append((7,
        b"<< /Length " + str(len(enc_stream)).encode()
        + b" /Filter /FlateDecode /Length1 " + str(len(ttf)).encode()
        + b" >>\nstream\n" + enc_stream + b"\nendstream"))
    objs.append((6,
        b"<< /Type /FontDescriptor /FontName /KasaneTest-Regular "
        b"/Flags 32 /FontBBox [0 -200 600 800] /ItalicAngle 0 "
        b"/Ascent 800 /Descent -200 /CapHeight 700 /StemV 80 "
        b"/FontFile2 7 0 R >>"))
    objs.append((5,
        b"<< /Type /Font /Subtype /TrueType /BaseFont /KasaneTest-Regular "
        b"/Encoding /WinAnsiEncoding "
        b"/FirstChar " + str(first).encode()
        + b" /LastChar " + str(last).encode()
        + b" /Widths " + warr
        + b" /FontDescriptor 6 0 R >>"))
    write("text_widths_override.pdf", build_pdf(objs))


if __name__ == "__main__":
    hello()
    content_ops()
    render_rects()
    render_annot()
    render_text()
    pages()
    incremental()
    xref_stream()
    encrypted()
    bad_offset()
    missing_ref()
    cyclic_pages()
    truncated()
    # M1
    flate_contents()
    contents_array()
    contents_none()
    flate_bad_data()
    flate_predictor()
    flate_predictor_bad()
    unsupported_filter()
    objstm()
    hybrid()
    # M1.1
    hybrid_compressed()
    hybrid_gap()
    prev_chain()
    xref_stream_bad_type()
    # M4
    xobject_form_basic()
    xobject_form_matrix()
    xobject_form_bbox_clip()
    xobject_form_own_resources()
    xobject_form_nested()
    xobject_form_self_recursive()
    xobject_form_group()
    xobject_image_gray_raw()
    xobject_image_rgb_flate()
    xobject_image_decode_invert()
    xobject_image_mask()
    xobject_image_smask()
    xobject_image_dct()
    xobject_unknown_subtype()
    xobject_missing()
    xobject_unused_bad()
    # M5
    text_basic()
    text_tj()
    text_quote_ops()
    text_tc_tw_tz_ts()
    text_render_modes()
    text_mode4_refused()
    text_in_form()
    text_same_name_two_resources()
    text_unsupported_type1()
    text_differences_encoding()
    text_notdef()
    text_unsupported_encoding()
    text_symbolic_refused()
    text_composite_glyph()
    text_rotated()
    text_scaled()
    text_clip()
    text_widths_override()
    text_extended_chars()
    text_tj_split()
    text_empty_string()
    text_tj_consecutive_numbers()
    text_missing_font_ref()
    text_no_font_tj()
