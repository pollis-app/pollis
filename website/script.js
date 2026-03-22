// ── Algorithms ────────────────────────────────────────────────────────────────

function pulsingWaveAlgorithm(grid, ctx) {
  const inc = ctx.deltaTime;
  const rows = grid.length;
  for (let y = 0; y < rows; y++) {
    const row = grid[y];
    const cols = row.length;
    for (let x = 0; x < cols; x++) {
      const cell = row[x];
      let phase = (cell.data ? cell.data.phase : Math.random() * Math.PI * 2) + inc;
      if (phase > 6.2832) { phase -= 6.2832; }
      cell.opacity = 0.3 + Math.sin(phase) * 0.4;
      cell.data = cell.data || {};
      cell.data.phase = phase;
    }
  }
  return grid;
}

function gameOfLifeAlgorithm(grid, ctx) {
  const rows = grid.length;
  if (rows === 0) { return grid; }
  const cols = grid[0].length;

  if (ctx.time < 0.05) {
    for (let y = 0; y < rows; y++) {
      for (let x = 0; x < cols; x++) {
        const alive = Math.random() > 0.7;
        grid[y][x].opacity = alive ? 1 : 0;
        grid[y][x].data = { alive, generation: 0 };
      }
    }
    return grid;
  }

  const updateInterval = 0.3;
  if (Math.floor(ctx.time / updateInterval) === Math.floor((ctx.time - ctx.deltaTime) / updateInterval)) {
    return grid;
  }

  const generation = ((grid[0][0].data && grid[0][0].data.generation) || 0) + 1;
  const reseed = generation % 30 === 0;

  // Copy alive state to avoid read-after-write
  const alive = [];
  for (let y = 0; y < rows; y++) {
    alive[y] = [];
    for (let x = 0; x < cols; x++) {
      alive[y][x] = grid[y][x].data ? grid[y][x].data.alive : false;
    }
  }

  for (let y = 0; y < rows; y++) {
    for (let x = 0; x < cols; x++) {
      let n = 0;
      for (let dy = -1; dy <= 1; dy++) {
        for (let dx = -1; dx <= 1; dx++) {
          if (dx === 0 && dy === 0) { continue; }
          if (alive[(y + dy + rows) % rows][(x + dx + cols) % cols]) { n++; }
        }
      }
      let newAlive = alive[y][x] ? (n === 2 || n === 3) : n === 3;
      if (reseed && Math.random() < 0.03) { newAlive = true; }
      grid[y][x].opacity = newAlive ? 1 : 0;
      grid[y][x].data = { alive: newAlive, generation };
    }
  }
  return grid;
}

function flowingWaveAlgorithm(grid, ctx) {
  const rows = grid.length;
  const t2 = ctx.time * 2;
  const pi2 = 6.2832;
  for (let y = 0; y < rows; y++) {
    const row = grid[y];
    const cols = row.length;
    for (let x = 0; x < cols; x++) {
      const wX = Math.sin((x / cols) * pi2 + t2) * 0.5 + 0.5;
      const wY = Math.cos((y / rows) * pi2 + t2) * 0.5 + 0.5;
      row[x].opacity = (wX + wY) * 0.5;
    }
  }
  return grid;
}

// ── DotMatrix ─────────────────────────────────────────────────────────────────

function createDotMatrix(options) {
  const dotSize = options.dotSize || 6;
  const spacing = options.spacing || 2;
  const color = options.defaultColor || "253, 186, 116";
  const speed = options.speed || 1.0;
  const algorithm = options.algorithm;
  const step = dotSize + spacing;

  const container = document.createElement('div');
  container.style.cssText = 'position:absolute;top:0;left:0;width:100%;height:100%;overflow:hidden;';

  const canvas = document.createElement('canvas');
  canvas.style.cssText = 'position:absolute;top:0;left:0;width:100%;height:100%;display:block;image-rendering:pixelated;';
  container.appendChild(canvas);

  let grid = [];
  let lastTime = 0;
  let startTime = 0;
  let rafId = null;
  let cw = 0;
  let ch = 0;
  let cols = 0;
  let rows = 0;

  function initGrid(w, h) {
    cols = Math.floor(w / step);
    rows = Math.floor(h / step);
    grid = [];
    for (let y = 0; y < rows; y++) {
      grid[y] = [];
      for (let x = 0; x < cols; x++) {
        grid[y][x] = { opacity: 0, data: null };
      }
    }
  }

  function draw() {
    if (rows === 0) { return; }
    const ctx = canvas.getContext('2d');
    if (!ctx) { return; }

    const offX = (cw - (cols * step - spacing)) * 0.5;
    const offY = (ch - (rows * step - spacing)) * 0.5;

    ctx.clearRect(0, 0, cw, ch);

    for (let y = 0; y < rows; y++) {
      const row = grid[y];
      const py = offY + y * step;
      for (let x = 0; x < cols; x++) {
        const op = row[x].opacity;
        if (op <= 0) { continue; }
        ctx.fillStyle = `rgba(${color},${op > 1 ? 1 : op})`;
        ctx.fillRect(offX + x * step, py, dotSize, dotSize);
      }
    }
  }

  function animate(ts) {
    if (startTime === 0) { startTime = ts; lastTime = ts; }
    const dt = Math.min((ts - lastTime) / 1000, 0.1) * speed;
    const time = (ts - startTime) / 1000;
    lastTime = ts;

    grid = algorithm(grid, { time, deltaTime: dt, speed, cols, rows });
    draw();
    rafId = requestAnimationFrame(animate);
  }

  function syncSize() {
    const rect = container.getBoundingClientRect();
    cw = Math.max(rect.width, 100);
    ch = Math.max(rect.height, 100);
    const dpr = window.devicePixelRatio || 1;
    canvas.width = cw * dpr;
    canvas.height = ch * dpr;
    const ctx = canvas.getContext('2d');
    if (ctx) { ctx.setTransform(dpr, 0, 0, dpr, 0, 0); }
    canvas.style.width = cw + 'px';
    canvas.style.height = ch + 'px';
    initGrid(cw, ch);
  }

  window.addEventListener('resize', syncSize);
  new ResizeObserver(syncSize).observe(container);

  function start() {
    syncSize();
    startTime = 0;
    rafId = requestAnimationFrame(animate);
  }

  function stop() {
    if (rafId) { cancelAnimationFrame(rafId); rafId = null; }
    window.removeEventListener('resize', syncSize);
  }

  return { element: container, start, stop };
}

// ── Init ──────────────────────────────────────────────────────────────────────

const bg = document.getElementById('dot-matrix-bg');

if (bg) {
  const algorithms = [gameOfLifeAlgorithm, pulsingWaveAlgorithm, flowingWaveAlgorithm];
  const pick = algorithms[Math.floor(Math.random() * algorithms.length)];

  const { element, start } = createDotMatrix({
    algorithm: pick,
    dotSize: 6,
    spacing: 2,
    defaultColor: '253, 186, 116',
    speed: 0.5,
  });

  bg.appendChild(element);
  start();
}
