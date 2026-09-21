// 生成应用图标(纯 Node 标准库,无第三方依赖)
// 用法: node scripts/gen-icons.mjs
import { deflateSync } from 'node:zlib';
import { mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..');
const OUT_DIR = join(ROOT, 'src-tauri', 'icons');

// ---------- PNG 编码 ----------
const CRC_TABLE = (() => {
  const t = new Int32Array(256);
  for (let n = 0; n < 256; n++) {
    let c = n;
    for (let k = 0; k < 8; k++) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    t[n] = c;
  }
  return t;
})();

function crc32(buf) {
  let c = -1;
  for (let i = 0; i < buf.length; i++) c = CRC_TABLE[(c ^ buf[i]) & 0xff] ^ (c >>> 8);
  return (c ^ -1) >>> 0;
}

function chunk(type, data) {
  const len = Buffer.alloc(4);
  len.writeUInt32BE(data.length);
  const typeBuf = Buffer.from(type, 'ascii');
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(Buffer.concat([typeBuf, data])));
  return Buffer.concat([len, typeBuf, data, crc]);
}

function encodePNG(w, h, rgba) {
  const stride = w * 4;
  const raw = Buffer.alloc((stride + 1) * h);
  for (let y = 0; y < h; y++) {
    raw[y * (stride + 1)] = 0; // filter: none
    rgba.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride);
  }
  const ihdr = Buffer.alloc(13);
  ihdr.writeUInt32BE(w, 0);
  ihdr.writeUInt32BE(h, 4);
  ihdr[8] = 8; // bit depth
  ihdr[9] = 6; // RGBA
  const idat = deflateSync(raw, { level: 9 });
  return Buffer.concat([
    Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]),
    chunk('IHDR', ihdr),
    chunk('IDAT', idat),
    chunk('IEND', Buffer.alloc(0)),
  ]);
}

// ---------- 绘制 ----------
function hex(c) {
  return [parseInt(c.slice(1, 3), 16), parseInt(c.slice(3, 5), 16), parseInt(c.slice(5, 7), 16)];
}

const BG_TOP = hex('#3b82f6');
const BG_BOTTOM = hex('#1d4ed8');
const BOARD = hex('#ffffff');
const CLIP = hex('#1e3a8a');
const LINES = hex('#3b82f6');

// 圆角矩形内测试(归一化坐标 0..1)
function inRRect(px, py, x0, y0, x1, y1, r) {
  if (px < x0 || px > x1 || py < y0 || py > y1) return false;
  const cx = Math.max(x0 + r, Math.min(px, x1 - r));
  const cy = Math.max(y0 + r, Math.min(py, y1 - r));
  const dx = px - cx;
  const dy = py - cy;
  return dx * dx + dy * dy <= r * r + 1e-9;
}

function draw(size) {
  const buf = Buffer.alloc(size * size * 4);
  const S = size;
  const put = (x, y, [r, g, b]) => {
    const i = (y * S + x) * 4;
    buf[i] = r;
    buf[i + 1] = g;
    buf[i + 2] = b;
    buf[i + 3] = 255;
  };
  for (let y = 0; y < S; y++) {
    for (let x = 0; x < S; x++) {
      const px = (x + 0.5) / S;
      const py = (y + 0.5) / S;
      // 背景渐变圆角矩形
      if (inRRect(px, py, 0.04, 0.04, 0.96, 0.96, 0.22)) {
        const t = py; // 垂直渐变
        const bg = [
          Math.round(BG_TOP[0] + (BG_BOTTOM[0] - BG_TOP[0]) * t),
          Math.round(BG_TOP[1] + (BG_BOTTOM[1] - BG_TOP[1]) * t),
          Math.round(BG_TOP[2] + (BG_BOTTOM[2] - BG_TOP[2]) * t),
        ];
        // 白色底板
        if (inRRect(px, py, 0.26, 0.2, 0.74, 0.82, 0.06)) {
          put(x, y, BOARD);
          // 内容行
          if (
            inRRect(px, py, 0.32, 0.36, 0.68, 0.4, 0.02) ||
            inRRect(px, py, 0.32, 0.48, 0.68, 0.52, 0.02) ||
            inRRect(px, py, 0.32, 0.6, 0.55, 0.64, 0.02)
          ) {
            put(x, y, LINES);
          }
        } else {
          put(x, y, bg);
        }
        // 顶部夹子(覆盖底板与背景)
        if (inRRect(px, py, 0.4, 0.13, 0.6, 0.26, 0.04)) {
          put(x, y, CLIP);
        }
      }
      // 圆角外透明(alpha 已默认 0)
    }
  }
  return encodePNG(S, S, buf);
}

mkdirSync(OUT_DIR, { recursive: true });
for (const size of [32, 128, 256, 512, 1024]) {
  writeFileSync(join(OUT_DIR, `${size}x${size}.png`), draw(size));
}
writeFileSync(join(OUT_DIR, 'icon.png'), draw(1024));
writeFileSync(join(OUT_DIR, 'tray.png'), draw(32));
console.log('图标已生成到 src-tauri/icons/');
