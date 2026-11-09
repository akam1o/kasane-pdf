#!/usr/bin/env python3
"""Fixed timing benchmark for `kasane-pdf redact`.

    scripts/bench-redact.py <kasane-pdf> [<kasane-pdf> …] [--runs N]

Generates the inputs (in a temporary directory; the font program is the
bundled LiberationSans) and redacts each with the same rect on page 1
(95 95 160 115, over "SECRET" at 100,100), N times sequentially per
binary (default 5), reporting the best wall time and the exit status.
Inputs: 400/800/1,600 fonts on one page; 200 pages in one font; 200
pages with 8 font dictionaries each (1,600, one program); 5,000 XObject
and ExtGState names; forms nested 15 deep; 50 pages of 20,000 shows;
4,000 annotations on two pages.
"""
import os
import subprocess
import sys
import tempfile
import time

def generate(FF, OUT):
    def build(objs):
        out=b"%PDF-1.7\n"; offs={}
        for n,b in objs: offs[n]=len(out); out+=b"%d 0 obj\n"%n+b+b"\nendobj\n"
        size=max(offs)+1; x=len(out)
        out+=b"xref\n0 %d\n0000000000 65535 f \n"%size+b"".join((b"%010d 00000 n \n"%offs[i]) if i in offs else b"0000000000 00000 f \n" for i in range(1,size))
        out+=b"trailer\n<< /Size %d /Root 1 0 R >>\nstartxref\n%d\n%%%%EOF\n"%(size,x); return out
    def stream(d,data): return b"<< %s /Length %d >>\nstream\n"%(d,len(data))+data+b"\nendstream"
    W=" ".join(["600"]*95)
    def font(n): return (n, ("<< /Type /Font /Subtype /TrueType /BaseFont /LiberationSans /FirstChar 32 /LastChar 126 /Widths [%s] /Encoding /WinAnsiEncoding /FontDescriptor 9 0 R >>"%W).encode())
    BASE=[(1,b"<< /Type /Catalog /Pages 2 0 R >>"),
          (9,b"<< /Type /FontDescriptor /FontName /LiberationSans /Flags 32 /FontBBox [-200 -300 1200 1000] /ItalicAngle 0 /Ascent 900 /Descent -200 /CapHeight 700 /StemV 80 /FontFile2 10 0 R >>"),
          (10,stream(b"/Length1 %d"%len(FF),FF))]
    SECRET=b"BT /F0 12 Tf 100 100 Td (SECRET) Tj ET "
    def doc(pages, extra=[]):
        # pages: list of (content bytes, resources bytes, page extra bytes)
        objs=list(BASE)+extra
        kids=[]
        for i,(c,res,pe) in enumerate(pages):
            n=100000+2*i; kids.append(b"%d 0 R"%n)
            objs.append((n,b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 600 800] /Resources << %s >> /Contents %d 0 R %s >>"%(res,n+1,pe)))
            objs.append((n+1,stream(b"",c)))
        objs.append((2,b"<< /Type /Pages /Kids [%s] /Count %d >>"%(b" ".join(kids),len(kids))))
        return build(sorted(objs))
    def write(name,data): open(f"{OUT}/{name}.pdf","wb").write(data)
    # 1. N distinct fonts on one page, each used once.
    for N in (400,800,1600):
        fonts=[font(20+i) for i in range(N)]
        res=b"/Font << "+b" ".join(b"/F%d %d 0 R"%(i,20+i) for i in range(N))+b" >>"
        c=SECRET+b"".join(b"BT /F%d 8 Tf %d %d Td (w%d) Tj ET "%(i,10+(i%20)*28,780-(i//20)*9,i) for i in range(1,N))
        write(f"fonts{N}",doc([(c,res,b"")],fonts))
    # 2. 200 pages, the same font, text on each.
    txt=lambda p: b"".join(b"BT /F0 10 Tf 40 %d Td (line %d of page %d with words) Tj ET "%(760-l*12,l,p) for l in range(50))
    write("pages200-samefont",doc([((SECRET if p==0 else b"")+txt(p),b"/Font << /F0 20 0 R >>",b"") for p in range(200)],[font(20)]))
    # 3. 200 pages, 8 distinct font dicts per page (1,600 font dicts, one program).
    pages=[]; fonts=[]
    for p in range(200):
        names=b" ".join(b"/F%d %d 0 R"%(k,20+p*8+k) for k in range(8)); fonts+= [font(20+p*8+k) for k in range(8)]
        c=(SECRET if p==0 else b"")+b"".join(b"BT /F%d 10 Tf 40 %d Td (font %d page %d) Tj ET "%(k,700-k*20,k,p) for k in range(8))
        pages.append((c,b"/Font << "+names+b" >>",b""))
    write("pages200-manyfonts",doc(pages,fonts))
    # 4. A /Resources with 5,000 unused XObject names and 5,000 ExtGStates.
    xo=[(20000+i,stream(b"/Type /XObject /Subtype /Form /BBox [0 0 1 1]",b"")) for i in range(5000)]
    res=b"/Font << /F0 20 0 R >> /XObject << "+b" ".join(b"/X%d %d 0 R"%(i,20000+i) for i in range(5000))+b" >> /ExtGState << "+b" ".join(b"/G%d << /CA 1 >>"%i for i in range(5000))+b" >>"
    write("bigresources",doc([(SECRET+txt(0)+b"/G7 gs /X3 Do",res,b"")],[font(20)]+xo))
    # 5. Forms nested 15 deep, text at each level.
    forms=[]
    for d in range(15):
        inner=b"/Fm%d Do"%(d+1) if d<14 else b""
        forms.append((30000+d,stream(b"/Type /XObject /Subtype /Form /BBox [0 0 600 800] /Resources << /Font << /F0 20 0 R >> /XObject << /Fm%d %d 0 R >> >>"%(d+1,30001+d),b"BT /F0 9 Tf 300 %d Td (level %d) Tj ET "%(700-d*15,d)+inner)))
    write("nestedforms",doc([(SECRET+b"/Fm0 Do",b"/Font << /F0 20 0 R >> /XObject << /Fm0 30000 0 R >>",b"")],[font(20)]+forms))
    # 6. Lots of text: 20,000 shows on page 1, 50 pages.
    big=b"".join(b"BT /F0 4 Tf %d %d Td (t%d) Tj ET "%(10+(i%100)*5.8,790-(i//100)*3.9,i%97) for i in range(20000))
    write("bigtext",doc([(SECRET+big if p==0 else big,b"/Font << /F0 20 0 R >>",b"") for p in range(50)],[font(20)]))
    # 7. 2,000 annotations on page 1 (clear of the rect), 2,000 on page 2.
    ann=[(40000+i,b"<< /Type /Annot /Subtype /Square /Rect [%d %d %d %d] /Contents (note %d) >>"%(300+i%50*5,400+i//50*5,302+i%50*5,402+i//50*5,i)) for i in range(4000)]
    a1=b"/Annots ["+b" ".join(b"%d 0 R"%(40000+i) for i in range(2000))+b"]"
    a2=b"/Annots ["+b" ".join(b"%d 0 R"%(42000+i) for i in range(2000))+b"]"
    write("annots",doc([(SECRET+txt(0),b"/Font << /F0 20 0 R >>",a1),(txt(1),b"/Font << /F0 20 0 R >>",a2)],[font(20)]+ann))


def main():
    args = sys.argv[1:]
    runs = 5
    if "--runs" in args:
        i = args.index("--runs")
        runs = int(args[i + 1])
        del args[i:i + 2]
    if not args:
        sys.exit(__doc__)
    here = os.path.dirname(os.path.abspath(__file__))
    ff = open(os.path.join(here, "..", "fonts", "liberation", "LiberationSans-Regular.ttf"), "rb").read()
    with tempfile.TemporaryDirectory() as tmp:
        generate(ff, tmp)
        out = os.path.join(tmp, "out", "o.pdf")
        os.makedirs(os.path.dirname(out))
        names = sorted(f for f in os.listdir(tmp) if f.endswith(".pdf"))
        print("input".ljust(22) + "".join(os.path.basename(b)[:14].rjust(16) for b in args))
        for f in names:
            row = []
            for b in args:
                best, rc = None, None
                for _ in range(runs):
                    t = time.perf_counter()
                    r = subprocess.run([b, "redact", os.path.join(tmp, f), "--page", "1", "--rect",
                                        "95", "95", "160", "115", "--output", out], capture_output=True)
                    d = time.perf_counter() - t
                    best = d if best is None else min(best, d)
                    rc = r.returncode
                row.append(f"{best:.3f}s" + ("" if rc == 0 else f" rc{rc}"))
            print(f[:-4].ljust(22) + "".join(c.rjust(16) for c in row))


if __name__ == "__main__":
    main()
