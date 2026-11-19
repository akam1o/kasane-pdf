// pdf.js page render for the synthetic Courier cases, with its standard font data (FoxitFixed for
// Courier), as a browser viewer draws a non-embedded Standard 14 font.
//   node pdfjs-render.mjs <pdfjs-dist dir> <in.pdf> <page> <dpi> <out.ppm>
import fs from 'node:fs';
import { pathToFileURL } from 'node:url';
const [dist, file, pageNo, dpi, out] = process.argv.slice(2);
const canvasMod = await import(pathToFileURL(dist + '/../@napi-rs/canvas/index.js').href);
const { createCanvas, DOMMatrix, ImageData, Path2D } = canvasMod.default ?? canvasMod;
Object.assign(globalThis, { DOMMatrix, ImageData, Path2D });
const { getDocument } = await import(pathToFileURL(dist + '/legacy/build/pdf.mjs').href);
const doc = await getDocument({
  data: new Uint8Array(fs.readFileSync(file)),
  disableFontFace: true,
  useSystemFonts: false,
  isEvalSupported: false,
  standardFontDataUrl: dist + '/standard_fonts/',
  verbosity: 0,
}).promise;
const page = await doc.getPage(Number(pageNo));
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
