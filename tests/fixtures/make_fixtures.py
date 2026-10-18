#!/usr/bin/env python3
"""Generate the hand-crafted PDF fixtures under tests/fixtures/.

The files are intentionally minimal but byte-accurate: xref offsets are
computed, not guessed. Regenerate with:

    python3 tests/fixtures/make_fixtures.py

Fixtures produced:

  hello.pdf            Minimal 1-page document, ASCII-only content stream.
  content_ops.pdf      Content stream exercising graphics-state, path,
                       colour, text and marked-content operators.
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
            b"/MediaBox [0 0 612 792] >>"),
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
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox 4 0 R >>"),
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
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"),
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
            b"/Contents [4 0 R 5 0 R] >>"),
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
    """Stream with a filter we don't implement (/LZWDecode)."""
    objects = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
            b"/Contents 4 0 R >>"),
        (4, b"<< /Length 10 /Filter /LZWDecode >>\nstream\n" + b"\x00" * 10 + b"\nendstream"),
    ]
    write("unsupported_filter.pdf", build_pdf(objects))


def objstm():
    """Page tree where the page object is inside an /ObjStm (object 5).
    Object 4 is the ObjStm; the page (5) is compressed inside it.
    Object 6 is the page's /Contents stream (regular object).
    The xref stream is used so entry types 1 and 2 can coexist."""
    page_dict = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 300 300] /Contents 6 0 R >>"
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
    page_dict = b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] >>"
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
        (5, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] >>"),
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


if __name__ == "__main__":
    hello()
    content_ops()
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
