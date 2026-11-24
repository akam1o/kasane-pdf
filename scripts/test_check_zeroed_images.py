#!/usr/bin/env python3
"""Synthetic cases for scripts/check-zeroed-images.py: a correctly zeroed
image is `zero`; a residue — one sample, a sample a palette draws black,
one 1-bit sample, text, a residue a soft mask, `ca 0`, optional content
or a /Decode hides — is ink; an image drawn intact elsewhere does not
count; data the check cannot read is `unhandled`, never `zero`. Skipped
without qpdf, pdftoppm and mutool."""
import importlib.util
import os
import shutil
import tempfile
import unittest
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
spec = importlib.util.spec_from_file_location("zc", os.path.join(HERE, "check-zeroed-images.py"))
zc = importlib.util.module_from_spec(spec)
spec.loader.exec_module(zc)
D = tempfile.mkdtemp(prefix="zc-")

def pdf(objs, path):
    out=b"%PDF-1.7\n"; offs=[]
    for i,o in enumerate(objs,1):
        offs.append(len(out)); out+=b"%d 0 obj\n"%i+o+b"\nendobj\n"
    x=len(out); out+=b"xref\n0 %d\n0000000000 65535 f \n"%(len(objs)+1)
    for o in offs: out+=b"%010d 00000 n \n"%o
    out+=b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n"%(len(objs)+1,x)
    open(path,"wb").write(out)
def stream(d,data,flate=True):
    if flate: data=zlib.compress(data); d+=b" /Filter /FlateDecode"
    return b"<< "+d+b" /Length %d >>\nstream\n"%len(data)+data+b"\nendstream"
RECT=(75,75,125,125)
def samples(n=3, zero=lambda x,y: 2<=x<=7 and 2<=y<=7, val=None):
    out=bytearray()
    for y in range(10):
        for x in range(10):
            v = 0 if zero(x,y) else (val(x,y) if val else 200)
            out+=bytes([v])*n
    return bytes(out)
def page(content, res=b"", extra=(), cat=b""):
    return [b"<< /Type /Catalog /Pages 2 0 R "+cat+b" >>", b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << "+res+b" >> /Contents 4 0 R >>",
            stream(b"",content,False)]+list(extra)
IMG=b"/Type /XObject /Subtype /Image /Width 10 /Height 10 /BitsPerComponent 8"
DRAW=b"q 100 0 0 100 50 50 cm /I Do Q"
CASES={}
CASES["zeroed"]=(page(DRAW,b"/XObject << /I 5 0 R >>",[stream(IMG+b" /ColorSpace /DeviceRGB",samples())]),"zero")
CASES["one residual sample"]=(page(DRAW,b"/XObject << /I 5 0 R >>",[stream(IMG+b" /ColorSpace /DeviceRGB",samples(zero=lambda x,y: 2<=x<=7 and 2<=y<=7 and (x,y)!=(4,4)))]),"ink")
CASES["residual sample that is black (Indexed)"]=(page(DRAW,b"/XObject << /I 5 0 R >>",[stream(IMG.replace(b"/BitsPerComponent 8",b"/BitsPerComponent 8")+b" /ColorSpace [/Indexed /DeviceRGB 1 <000000000000>]",samples(n=1,zero=lambda x,y: 2<=x<=7 and 2<=y<=7 and (x,y)!=(4,4),val=lambda x,y:1))]),"ink")
CASES["black text left"]=(page(DRAW+b" BT /F1 20 Tf 90 95 Td (X) Tj ET",b"/XObject << /I 5 0 R >> /Font << /F1 6 0 R >>",[stream(IMG+b" /ColorSpace /DeviceRGB",samples()),b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"]),"ink")
CASES["residual hidden by an SMask"]=(page(DRAW,b"/XObject << /I 5 0 R >>",[stream(IMG+b" /ColorSpace /DeviceRGB /SMask 6 0 R",samples(zero=lambda x,y: False)),stream(IMG+b" /ColorSpace /DeviceGray",samples(n=1,zero=lambda x,y: 2<=x<=7 and 2<=y<=7,val=lambda x,y:255))]),"ink")
CASES["residual under ca 0"]=(page(b"/G0 gs "+DRAW,b"/XObject << /I 5 0 R >> /ExtGState << /G0 << /Type /ExtGState /ca 0 /CA 0 >> >>",[stream(IMG+b" /ColorSpace /DeviceRGB",samples(zero=lambda x,y: False))]),"ink")
CASES["residual under optional content off"]=(page(b"/OC /L BDC "+DRAW+b" EMC",b"/XObject << /I 5 0 R >> /Properties << /L 6 0 R >>",[stream(IMG+b" /ColorSpace /DeviceRGB",samples(zero=lambda x,y: False)),b"<< /Type /OCG /Name (l) >>"],cat=b"/OCProperties << /OCGs [6 0 R] /D << /OFF [6 0 R] >> >>"),"ink")
CASES["residual under a /Decode that whitens it"]=(page(DRAW,b"/XObject << /I 5 0 R >>",[stream(IMG+b" /ColorSpace /DeviceGray /Decode [1 0]",samples(n=1,zero=lambda x,y: 2<=x<=7 and 2<=y<=7 and (x,y)!=(4,4),val=lambda x,y:255))]),"ink")
CASES["the same image drawn elsewhere, intact"]=(page(DRAW+b" q 40 0 0 40 155 5 cm /J Do Q",b"/XObject << /I 5 0 R /J 6 0 R >>",[stream(IMG+b" /ColorSpace /DeviceRGB",samples()),stream(IMG+b" /ColorSpace /DeviceRGB",samples(zero=lambda x,y:False))]),"zero")
CASES["an undecodable filter"]=(page(DRAW,b"/XObject << /I 5 0 R >>",[b"<< "+IMG+b" /ColorSpace /DeviceGray /Filter /JBIG2Decode /Length 4 >>\nstream\n\x00\x01\x02\x03\nendstream"]),"unhandled")
CASES["an inline image"]=(page(b"q 100 0 0 100 50 50 cm BI /W 1 /H 1 /CS /G /BPC 8 ID \x80 EI Q"),"unhandled")
CASES["short image data"]=(page(DRAW,b"/XObject << /I 5 0 R >>",[stream(IMG+b" /ColorSpace /DeviceRGB",samples()[:50])]),"unhandled")
CASES["16-bit and 1-bit, zeroed"]=(page(DRAW+b" q 100 0 0 100 50 50 cm /J Do Q",b"/XObject << /I 5 0 R /J 6 0 R >>",[stream(IMG.replace(b"8",b"16")+b" /ColorSpace /DeviceGray",samples(n=2)),stream(IMG.replace(b"/BitsPerComponent 8",b"/BitsPerComponent 1")+b" /ColorSpace /DeviceGray",bytes([0b11000000,0b11]*10))]),"zero")
CASES["1-bit, one residual bit"]=(page(DRAW,b"/XObject << /I 5 0 R >>",[stream(IMG.replace(b"/BitsPerComponent 8",b"/BitsPerComponent 1")+b" /ColorSpace /DeviceGray",bytes([0b11000000,0b11]*4+[0b11001000,0b11]+[0b11000000,0b11]*5))]),"ink")


TOOLS = all(shutil.which(t) for t in ("qpdf", "pdftoppm", "mutool"))


@unittest.skipUnless(TOOLS or os.environ.get("KASANE_REQUIRE_TOOLS"), "qpdf, pdftoppm and mutool")
class ZeroedImages(unittest.TestCase):
    def test_cases(self):
        for name, (objs, want) in CASES.items():
            with self.subTest(name):
                p = os.path.join(D, name.replace(" ", "_").replace("/", "") + ".pdf")
                pdf(objs, p)
                self.assertEqual(zc.check(p, 1, RECT).split(":")[0], want)


if __name__ == "__main__":
    unittest.main()
