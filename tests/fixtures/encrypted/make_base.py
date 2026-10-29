import sys
xmp=b"<x:xmpmeta xmlns:x='adobe:ns:meta/'>plain metadata</x:xmpmeta>"
objs=[b"<< /Type /Catalog /Pages 2 0 R /Metadata 7 0 R >>",
 b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
 b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 100] /Resources << /Font << /F1 5 0 R >> >> /Contents 4 0 R >>",
 None,
 b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>",
 b"<< /Title (Secret title) /Producer (kasane-pdf test) >>",
 b"<< /Type /Metadata /Subtype /XML /Length %d >>\nstream\n"%len(xmp)+xmp+b"\nendstream"]
content=b"0 0 1 rg 10 10 50 50 re f BT /F1 12 Tf 70 40 Td (Hello encrypted) Tj ET"
objs[3]=b"<< /Length %d >>\nstream\n"%len(content)+content+b"\nendstream"
out=bytearray(b"%PDF-1.7\n"); offs=[]
for i,o in enumerate(objs):
    offs.append(len(out)); out+=b"%d 0 obj\n"%(i+1)+o+b"\nendobj\n"
x=len(out); out+=b"xref\n0 %d\n0000000000 65535 f \n"%(len(objs)+1)
for o in offs: out+=b"%010d 00000 n \n"%o
out+=b"trailer\n<< /Size %d /Root 1 0 R /Info 6 0 R /ID [<00112233445566778899AABBCCDDEEFF><00112233445566778899AABBCCDDEEFF>] >>\nstartxref\n%d\n%%%%EOF\n"%(len(objs)+1,x)
open(sys.argv[1],'wb').write(out)
