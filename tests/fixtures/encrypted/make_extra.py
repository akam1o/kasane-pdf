"""Encrypted fixtures qpdf cannot produce, written with an implementation
independent of kasane-pdf's (Python, hashlib, a textbook RC4):

- enc_r4_rc4_40_spec.pdf: R4, /V2 crypt filter with a 40-bit key; object
  keys derived from the 5-byte key as ISO 32000 Algorithm 1 specifies.
  The content stream is /FlateDecode, so a reader can tell which key
  reading decrypts it.
- enc_r4_rc4_40_acrobat.pdf: the same, but objects encrypted the way
  Acrobat does it: the 5-byte key zero-padded to 16 bytes before
  Algorithm 1 (undocumented; pdf.js issue 19484).
- enc_r2_incremental.pdf: enc_r2_rc4_40.pdf plus an incremental update
  that replaces the content stream and the /Info title, encrypted with
  the file key qpdf reports for that file (91a6add96f).

User password empty, owner password "own". Run from this directory:
    python3 make_extra.py
"""

import hashlib
import re
import zlib

PAD = bytes.fromhex("28BF4E5E4E758A4164004E56FFFA01082E2E00B6D0683E802F0CA9FE6453697A")
ID0 = bytes.fromhex("00112233445566778899aabbccddeeff")
ID1 = bytes.fromhex("31415926535897932384626433832795")


def rc4(key, data):
    s = list(range(256))
    j = 0
    for i in range(256):
        j = (j + s[i] + key[i % len(key)]) % 256
        s[i], s[j] = s[j], s[i]
    i = j = 0
    out = bytearray()
    for c in data:
        i = (i + 1) % 256
        j = (j + s[i]) % 256
        s[i], s[j] = s[j], s[i]
        out.append(c ^ s[(s[i] + s[j]) % 256])
    return bytes(out)


def md5(b):
    return hashlib.md5(b).digest()


def pad(pw):
    return (pw + PAD)[:32]


def owner_value(owner, user, n):
    """Algorithm 3 (R3+)."""
    h = md5(pad(owner))
    for _ in range(50):
        h = md5(h)
    key = h[:n]
    out = rc4(key, pad(user))
    for i in range(1, 20):
        out = rc4(bytes(b ^ i for b in key), out)
    return out


def file_key(user, o, p, n, encrypt_metadata):
    """Algorithm 2."""
    h = md5(pad(user) + o + (p & 0xFFFFFFFF).to_bytes(4, "little") + ID0
            + (b"" if encrypt_metadata else b"\xff\xff\xff\xff"))
    for _ in range(50):
        h = md5(h[:n])
    return h[:n]


def user_value(key):
    """Algorithm 5."""
    out = rc4(key, md5(PAD + ID0))
    for i in range(1, 20):
        out = rc4(bytes(b ^ i for b in key), out)
    return out + bytes(16)


def object_key(key, num, gen=0):
    """Algorithm 1 (RC4)."""
    return md5(key + num.to_bytes(3, "little") + gen.to_bytes(2, "little"))[:min(len(key) + 5, 16)]


def hexs(b):
    return b"<" + b.hex().encode() + b">"


def build(objs, trailer):
    out = bytearray(b"%PDF-1.6\n")
    offs = {}
    for num in sorted(objs):
        offs[num] = len(out)
        out += b"%d 0 obj\n" % num + objs[num] + b"\nendobj\n"
    size = max(objs) + 1
    x = len(out)
    out += b"xref\n0 %d\n0000000000 65535 f \n" % size
    for n in range(1, size):
        out += (b"%010d 00000 n \n" % offs[n]) if n in offs else b"0000000000 00000 f \n"
    out += b"trailer\n" + trailer % size + b"\nstartxref\n%d\n%%%%EOF\n" % x
    return bytes(out)


def stream(num, key, data, extra=b""):
    enc = rc4(object_key(key, num), data)
    return b"<< /Length %d%s >>\nstream\n" % (len(enc), extra) + enc + b"\nendstream"


def short_key_file(acrobat):
    n, p = 5, -4
    o = owner_value(b"own", b"", n)
    key = file_key(b"", o, p, n, True)
    u = user_value(key)
    obj_key_base = key + bytes(16 - n) if acrobat else key
    content = b"0 0 1 rg 10 10 50 50 re f BT /F1 12 Tf 70 40 Td (Hello encrypted) Tj ET"
    title = rc4(object_key(obj_key_base, 6), b"Secret title")
    packed = zlib.compress(content)
    objs = {
        1: b"<< /Type /Catalog /Pages 2 0 R >>",
        2: b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
        3: b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] "
           b"/Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
        4: stream(4, obj_key_base, packed, b" /Filter /FlateDecode"),
        5: b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
        6: b"<< /Title " + hexs(title) + b" >>",
        7: b"<< /Filter /Standard /V 4 /R 4 /Length 40 /P %d /O " % p + hexs(o)
           + b" /U " + hexs(u)
           + b" /CF << /StdCF << /CFM /V2 /AuthEvent /DocOpen /Length 5 >> >>"
           + b" /StmF /StdCF /StrF /StdCF >>",
    }
    trailer = (b"<< /Size %d /Root 1 0 R /Info 6 0 R /Encrypt 7 0 R /ID ["
               + hexs(ID0).replace(b"%", b"%%") + hexs(ID1) + b"] >>")
    return build(objs, trailer)


def incremental():
    base = open("enc_r2_rc4_40.pdf", "rb").read()
    key = bytes.fromhex("91a6add96f")
    content = b"1 0 0 rg 10 10 50 50 re f BT /F1 12 Tf 70 40 Td (Updated revision) Tj ET"
    title = rc4(object_key(key, 2), b"Updated title")
    prev = int(re.findall(rb"startxref\s+(\d+)", base)[-1])
    out = bytearray(base)
    offs = {}
    offs[6] = len(out)
    out += b"6 0 obj\n" + stream(6, key, content) + b"\nendobj\n"
    offs[2] = len(out)
    out += b"2 0 obj\n<< /Title " + hexs(title) + b" >>\nendobj\n"
    x = len(out)
    out += b"xref\n2 1\n%010d 00000 n \n6 1\n%010d 00000 n \n" % (offs[2], offs[6])
    out += (b"trailer\n<< /Size 9 /Root 1 0 R /Info 2 0 R /Encrypt 8 0 R /Prev %d /ID ["
            % prev + hexs(ID0) + hexs(ID1) + b"] >>\nstartxref\n%d\n%%%%EOF\n" % x)
    return bytes(out)


if __name__ == "__main__":
    open("enc_r4_rc4_40_spec.pdf", "wb").write(short_key_file(False))
    open("enc_r4_rc4_40_acrobat.pdf", "wb").write(short_key_file(True))
    open("enc_r2_incremental.pdf", "wb").write(incremental())
