// pdf.js as a reference for scripts/verify-renderers.py.
//
//   node render.mjs render <in.pdf> <dpi> <out.ppm>   page 1 as binary PPM (RGB on white)
//   node render.mjs text <in.pdf>                       page 1's text, one item per line
//   node render.mjs version                             pdfjs-dist's version
import fs from 'node:fs';
import { createRequire } from 'node:module';
import { createCanvas, DOMMatrix, ImageData, Path2D } from '@napi-rs/canvas';

Object.assign(globalThis, { DOMMatrix, ImageData, Path2D });
const [mode, file, dpi, out] = process.argv.slice(2);
if (mode === 'version') {
  const require = createRequire(import.meta.url);
  console.log(require('pdfjs-dist/package.json').version);
  process.exit(0);
}
const { getDocument } = await import('pdfjs-dist/legacy/build/pdf.mjs');
const doc = await getDocument({
  data: new Uint8Array(fs.readFileSync(file)),
  disableFontFace: true,
  useSystemFonts: false,
  isEvalSupported: false,
  verbosity: 0,
}).promise;
const page = await doc.getPage(1);
if (mode === 'text') {
  const content = await page.getTextContent();
  process.stdout.write(content.items.map((i) => i.str).join('\n') + '\n');
} else if (mode === 'render') {
  const v = page.getViewport({ scale: Number(dpi) / 72 });
  const c = createCanvas(Math.ceil(v.width), Math.ceil(v.height));
  const ctx = c.getContext('2d');
  ctx.fillStyle = '#ffffff';
  ctx.fillRect(0, 0, c.width, c.height);
  await page.render({ canvasContext: ctx, viewport: v }).promise;
  const rgba = ctx.getImageData(0, 0, c.width, c.height).data;
  const rgb = Buffer.alloc(c.width * c.height * 3);
  for (let i = 0; i < c.width * c.height; i++) for (let k = 0; k < 3; k++) rgb[i * 3 + k] = rgba[i * 4 + k];
  fs.writeFileSync(out, Buffer.concat([Buffer.from(`P6\n${c.width} ${c.height}\n255\n`), rgb]));
} else {
  console.error('usage: render.mjs render|text|version ...');
  process.exit(2);
}
