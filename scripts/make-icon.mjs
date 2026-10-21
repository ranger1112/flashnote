// 生成一张 1024x1024 的闪记应用图标（不依赖任何第三方库）。
import { writeFileSync } from "node:fs";
import { deflateSync } from "node:zlib";

const SIZE = 1024;
const SAMPLES = 4;

const layers = [
  { type: "outer", x: 64, y: 64, w: 896, h: 896, r: 200, color: [23, 23, 27] },
  { type: "card", x: 236, y: 210, w: 552, h: 604, r: 72, color: [250, 250, 248] },
  { type: "bar", x: 316, y: 360, w: 392, h: 36, r: 18, color: [42, 42, 48] },
  { type: "bar", x: 316, y: 452, w: 316, h: 36, r: 18, color: [42, 42, 48] },
  { type: "bar", x: 316, y: 544, w: 356, h: 36, r: 18, color: [42, 42, 48] },
  { type: "bar", x: 316, y: 636, w: 180, h: 36, r: 18, color: [245, 165, 36] },
];

function insideRoundedRect(x, y, rect) {
  const innerX = Math.max(rect.x + rect.r, Math.min(x, rect.x + rect.w - rect.r));
  const innerY = Math.max(rect.y + rect.r, Math.min(y, rect.y + rect.h - rect.r));
  const dx = x - innerX;
  const dy = y - innerY;
  return dx * dx + dy * dy <= rect.r * rect.r;
}

const rgba = Buffer.alloc(SIZE * SIZE * 4);

for (let py = 0; py < SIZE; py += 1) {
  for (let px = 0; px < SIZE; px += 1) {
    let r = 0;
    let g = 0;
    let b = 0;
    let a = 0;

    for (let sy = 0; sy < SAMPLES; sy += 1) {
      for (let sx = 0; sx < SAMPLES; sx += 1) {
        const x = px + (sx + 0.5) / SAMPLES;
        const y = py + (sy + 0.5) / SAMPLES;
        let hit = null;
        for (const layer of layers) {
          if (insideRoundedRect(x, y, layer)) hit = layer;
        }
        if (hit) {
          r += hit.color[0];
          g += hit.color[1];
          b += hit.color[2];
          a += 255;
        }
      }
    }

    const total = SAMPLES * SAMPLES;
    const index = (py * SIZE + px) * 4;
    rgba[index] = Math.round(r / total);
    rgba[index + 1] = Math.round(g / total);
    rgba[index + 2] = Math.round(b / total);
    rgba[index + 3] = Math.round(a / total);
  }
}

const crcTable = [];
for (let n = 0; n < 256; n += 1) {
  let c = n;
  for (let k = 0; k < 8; k += 1) {
    c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  }
  crcTable.push(c >>> 0);
}

function crc32(buffer) {
  let c = 0xffffffff;
  for (const byte of buffer) {
    c = crcTable[(c ^ byte) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

function chunk(type, data) {
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const typeBuffer = Buffer.from(type, "ascii");
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuffer, data])), 0);
  return Buffer.concat([length, typeBuffer, data, crc]);
}

const ihdr = Buffer.alloc(13);
ihdr.writeUInt32BE(SIZE, 0);
ihdr.writeUInt32BE(SIZE, 4);
ihdr[8] = 8;
ihdr[9] = 6;
ihdr[10] = 0;
ihdr[11] = 0;
ihdr[12] = 0;

const raw = Buffer.alloc((SIZE * 4 + 1) * SIZE);
for (let y = 0; y < SIZE; y += 1) {
  const rowStart = y * (SIZE * 4 + 1);
  raw[rowStart] = 0;
  rgba.copy(raw, rowStart + 1, y * SIZE * 4, (y + 1) * SIZE * 4);
}

const png = Buffer.concat([
  Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
  chunk("IHDR", ihdr),
  chunk("IDAT", deflateSync(raw, { level: 9 })),
  chunk("IEND", Buffer.alloc(0)),
]);

writeFileSync(new URL("../app-icon.png", import.meta.url), png);
console.log("app-icon.png written");
