#!/usr/bin/env python3
"""Generate the hand-crafted PDF fixtures under tests/fixtures/.

The files are intentionally minimal but byte-accurate: xref offsets are
computed, not guessed. Regenerate with:

    python3 tests/fixtures/make_fixtures.py

Fixtures produced:

  hello.pdf            Minimal 1-page document, ASCII-only content stream.
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


if __name__ == "__main__":
    hello()
    pages()
    incremental()
    xref_stream()
    encrypted()
    bad_offset()
    missing_ref()
    cyclic_pages()
    truncated()
