// ── DotMatrix ─────────────────────────────────────────────────────────────────
//
// The canvas holds ONE PIXEL PER CELL. Each frame writes cell intensities into
// a Uint32 view of an ImageData and does a single small putImageData; the
// compositor scales the canvas up with nearest-neighbour sampling and a CSS
// mask of repeating squares cuts the scaled cells into dots. No per-dot canvas
// calls and no per-frame allocation.

const ALGORITHMS = ['gameOfLife', 'pulsingWave', 'flowingWave'];

// Below this intensity a cell is drawn fully transparent.
const MIN_VISIBLE_OPACITY = 0.02;

function createDotMatrix(options) {
  const dotSize = options.dotSize || 6;
  const spacing = options.spacing || 2;
  const rgb = options.rgb || [253, 186, 116];
  const speed = options.speed || 1.0;
  const kind = options.algorithm;
  const step = dotSize + spacing;
  const half = spacing / 2;
  // ImageData is RGBA in memory, which is ABGR when read as a little-endian u32.
  const color = (rgb[2] << 16) | (rgb[1] << 8) | rgb[0];

  const container = document.createElement('div');
  container.style.cssText = 'position:absolute;top:0;left:0;width:100%;height:100%;overflow:hidden;';

  const canvas = document.createElement('canvas');
  const mask = 'url("data:image/svg+xml,' + encodeURIComponent(
    "<svg xmlns='http://www.w3.org/2000/svg' width='" + step + "' height='" + step + "'><rect width='" + dotSize + "' height='" + dotSize + "'/></svg>"
  ) + '")';
  canvas.style.cssText =
    'position:absolute;display:block;image-rendering:crisp-edges;image-rendering:pixelated;' +
    '-webkit-mask-image:' + mask + ';mask-image:' + mask + ';' +
    '-webkit-mask-size:' + step + 'px ' + step + 'px;mask-size:' + step + 'px ' + step + 'px;' +
    '-webkit-mask-position:' + half + 'px ' + half + 'px;mask-position:' + half + 'px ' + half + 'px;' +
    '-webkit-mask-repeat:repeat;mask-repeat:repeat;';
  container.appendChild(canvas);
  const ctx = canvas.getContext('2d');

  let cols = 0;
  let rows = 0;
  let opacity = new Float32Array(0);
  let aliveA = new Uint8Array(0);
  let aliveB = new Uint8Array(0);
  let phase = new Float32Array(0);
  let colWave = new Float32Array(0);
  let rowWave = new Float32Array(0);
  let image = null;
  let pixels = new Uint32Array(0);

  let startTime = 0;
  let lastTime = 0;
  let lastGolUpdate = 0;
  let generation = 0;
  let dirty = true;
  let rafId = null;

  function allocate(nextCols, nextRows) {
    const total = nextCols * nextRows;
    const nOpacity = new Float32Array(total);
    const nAliveA = new Uint8Array(total);
    const nPhase = new Float32Array(total);

    // Copy the overlapping region so resizes don't restart the animation.
    if (cols > 0 && rows > 0) {
      const copyCols = Math.min(cols, nextCols);
      const copyRows = Math.min(rows, nextRows);
      for (let y = 0; y < copyRows; y++) {
        for (let x = 0; x < copyCols; x++) {
          nOpacity[y * nextCols + x] = opacity[y * cols + x];
          nAliveA[y * nextCols + x] = aliveA[y * cols + x];
          nPhase[y * nextCols + x] = phase[y * cols + x];
        }
      }
    } else {
      for (let i = 0; i < total; i++) {
        nPhase[i] = Math.random() * Math.PI * 2;
        nAliveA[i] = Math.random() > 0.7 ? 1 : 0;
        nOpacity[i] = nAliveA[i];
      }
    }

    opacity = nOpacity;
    aliveA = nAliveA;
    aliveB = new Uint8Array(total);
    phase = nPhase;
    cols = nextCols;
    rows = nextRows;

    canvas.width = Math.max(1, cols);
    canvas.height = Math.max(1, rows);
    image = total > 0 && ctx ? ctx.createImageData(cols, rows) : null;
    pixels = image ? new Uint32Array(image.data.buffer) : new Uint32Array(0);
  }

  function syncSize() {
    const rect = container.getBoundingClientRect();
    const w = Math.max(rect.width, 100);
    const h = Math.max(rect.height, 100);
    const nextCols = Math.floor(w / step);
    const nextRows = Math.floor(h / step);
    // Shift back by half the gap so cell edges land inside the masked gaps.
    canvas.style.left = ((w - nextCols * step) / 2 - half) + 'px';
    canvas.style.top = ((h - nextRows * step) / 2 - half) + 'px';
    canvas.style.width = (nextCols * step) + 'px';
    canvas.style.height = (nextRows * step) + 'px';
    if (nextCols !== cols || nextRows !== rows) {
      allocate(nextCols, nextRows);
      dirty = true;
    }
  }

  function stepPulsingWave(dt) {
    const len = cols * rows;
    for (let i = 0; i < len; i++) {
      let p = phase[i] + dt;
      if (p > 6.2832) { p -= 6.2832; }
      phase[i] = p;
      opacity[i] = 0.3 + Math.sin(p) * 0.4;
    }
  }

  function stepFlowingWave(time) {
    if (colWave.length !== cols) { colWave = new Float32Array(cols); }
    if (rowWave.length !== rows) { rowWave = new Float32Array(rows); }
    const t2 = time * 2;
    for (let x = 0; x < cols; x++) {
      colWave[x] = Math.sin((x / cols) * 6.2832 + t2) * 0.5 + 0.5;
    }
    for (let y = 0; y < rows; y++) {
      rowWave[y] = Math.cos((y / rows) * 6.2832 + t2) * 0.5 + 0.5;
    }
    for (let y = 0; y < rows; y++) {
      const base = y * cols;
      const ry = rowWave[y];
      for (let x = 0; x < cols; x++) {
        opacity[base + x] = (colWave[x] + ry) * 0.5;
      }
    }
  }

  function stepGameOfLife() {
    generation++;
    const reseed = generation % 30 === 0;
    for (let y = 0; y < rows; y++) {
      const yUp = (y === 0 ? rows - 1 : y - 1) * cols;
      const yDn = (y === rows - 1 ? 0 : y + 1) * cols;
      const yMid = y * cols;
      for (let x = 0; x < cols; x++) {
        const xL = x === 0 ? cols - 1 : x - 1;
        const xR = x === cols - 1 ? 0 : x + 1;
        const n =
          aliveA[yUp + xL] + aliveA[yUp + x] + aliveA[yUp + xR] +
          aliveA[yMid + xL] + aliveA[yMid + xR] +
          aliveA[yDn + xL] + aliveA[yDn + x] + aliveA[yDn + xR];
        let next = aliveA[yMid + x] ? (n === 2 || n === 3 ? 1 : 0) : (n === 3 ? 1 : 0);
        if (reseed && Math.random() < 0.03) { next = 1; }
        aliveB[yMid + x] = next;
        opacity[yMid + x] = next;
      }
    }
    const swap = aliveA;
    aliveA = aliveB;
    aliveB = swap;
  }

  function draw() {
    if (!image) { return; }
    const len = cols * rows;
    for (let i = 0; i < len; i++) {
      const op = opacity[i];
      if (op < MIN_VISIBLE_OPACITY) {
        pixels[i] = 0;
        continue;
      }
      const a = op >= 1 ? 255 : (op * 255) | 0;
      pixels[i] = (a << 24) | color;
    }
    ctx.putImageData(image, 0, 0);
  }

  function animate(ts) {
    rafId = requestAnimationFrame(animate);
    if (startTime === 0) { startTime = ts; lastTime = ts; lastGolUpdate = ts; }
    const dt = Math.min((ts - lastTime) / 1000, 0.1) * speed;
    const time = (ts - startTime) / 1000;
    lastTime = ts;

    if (kind === 'pulsingWave') {
      stepPulsingWave(dt);
      dirty = true;
    } else if (kind === 'flowingWave') {
      stepFlowingWave(time);
      dirty = true;
    } else if (ts - lastGolUpdate > 300) {
      // Game of life ticks on a fixed cadence; frames in between draw nothing.
      stepGameOfLife();
      lastGolUpdate = ts;
      dirty = true;
    }

    if (dirty) {
      draw();
      dirty = false;
    }
  }

  const ro = new ResizeObserver(syncSize);
  ro.observe(container);

  function start() {
    syncSize();
    startTime = 0;
    rafId = requestAnimationFrame(animate);
  }

  function stop() {
    if (rafId) { cancelAnimationFrame(rafId); rafId = null; }
    ro.disconnect();
  }

  return { element: container, start, stop };
}

// ── Init ──────────────────────────────────────────────────────────────────────

const bg = document.getElementById('dot-matrix-bg');

if (bg) {
  const { element, start } = createDotMatrix({
    algorithm: ALGORITHMS[Math.floor(Math.random() * ALGORITHMS.length)],
    dotSize: 6,
    spacing: 2,
    rgb: [253, 186, 116],
    speed: 0.5,
  });

  bg.appendChild(element);
  start();
}
