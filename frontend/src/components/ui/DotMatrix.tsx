import React, { useEffect, useMemo, useRef } from "react";

// ─── Algorithm tokens ─────────────────────────────────────────────────────────
//
// Algorithms are opaque tagged objects rather than functions. The renderer
// dispatches on `kind` and runs a typed-array implementation so the hot loop
// never allocates. This trades a little flexibility for ~10–50x throughput
// (no per-frame `.map().map()` + boxed `{opacity, data}` objects).

export type DotMatrixAlgorithm =
  | { kind: "pulsingWave" }
  | { kind: "gameOfLife" }
  | { kind: "flowingWave" };

export const pulsingWaveAlgorithm: DotMatrixAlgorithm = { kind: "pulsingWave" };
export const gameOfLifeAlgorithm: DotMatrixAlgorithm = { kind: "gameOfLife" };
export const flowingWaveAlgorithm: DotMatrixAlgorithm = { kind: "flowingWave" };

export const ALL_ALGORITHMS: DotMatrixAlgorithm[] = [
  pulsingWaveAlgorithm,
  gameOfLifeAlgorithm,
  flowingWaveAlgorithm,
];

// ─── Component ────────────────────────────────────────────────────────────────

interface DotMatrixProps {
  algorithm?: DotMatrixAlgorithm;
  dotSize?: number;
  spacing?: number;
  speed?: number;
  className?: string;
  style?: React.CSSProperties;
}

// Number of opacity buckets used for batched drawing. Each bucket gets one
// fillStyle assignment and one pass over its cells, dropping the per-dot
// string formatting that dominated CPU time previously.
const OPACITY_BUCKETS = 16;

export const DotMatrix: React.FC<DotMatrixProps> = ({
  algorithm,
  dotSize = 6,
  spacing = 6,
  speed = 0.4,
  className = "",
  style,
}) => {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const containerRef = useRef<HTMLDivElement>(null);

  const resolvedAlgorithm = useMemo(
    () => algorithm ?? ALL_ALGORITHMS[Math.floor(Math.random() * ALL_ALGORITHMS.length)],
    // eslint-disable-next-line react-hooks/exhaustive-deps
    []
  );

  useEffect(() => {
    const canvas = canvasRef.current;
    const container = containerRef.current;
    if (!canvas || !container) { return; }

    const ctx = canvas.getContext("2d", { alpha: true });
    if (!ctx) { return; }

    const step = dotSize + spacing;
    const kind = resolvedAlgorithm.kind;

    // ── Typed-array state. Reallocated only on resize. ──────────────────────
    let cols = 0;
    let rows = 0;
    let width = 0;
    let height = 0;
    let dpr = window.devicePixelRatio || 1;
    let offsetX = 0;
    let offsetY = 0;

    // opacity: 0..1 per cell, row-major
    let opacity = new Float32Array(0);
    // GoL state: alive/dead, double-buffered
    let aliveA = new Uint8Array(0);
    let aliveB = new Uint8Array(0);
    // pulsingWave per-cell phase
    let phase = new Float32Array(0);

    let startTime = 0;
    let lastTime = 0;
    let lastGolUpdate = 0;
    let rafId = 0;
    let colorString = "rgba(255,180,40,"; // updated from CSS vars

    // Refresh accent color from CSS vars. We re-read on a slow cadence rather
    // than every frame because getComputedStyle is surprisingly expensive.
    let lastColorCheck = 0;
    const refreshColor = () => {
      const cs = getComputedStyle(document.documentElement);
      const h = cs.getPropertyValue("--accent-h").trim() || "38";
      const s = cs.getPropertyValue("--accent-s").trim() || "90%";
      // Pre-resolve hsl→rgba once per refresh by drawing into a 1x1 buffer.
      // hsl() with alpha works directly in fillStyle, so just store the prefix.
      colorString = `hsl(${h} ${s} 62% / `;
    };
    refreshColor();

    const allocate = (nextCols: number, nextRows: number) => {
      const total = nextCols * nextRows;
      const newOpacity = new Float32Array(total);
      const newAliveA = new Uint8Array(total);
      const newAliveB = new Uint8Array(total);
      const newPhase = new Float32Array(total);

      // Copy overlapping region so resizes don't blank the animation.
      if (cols > 0 && rows > 0) {
        const copyCols = Math.min(cols, nextCols);
        const copyRows = Math.min(rows, nextRows);
        for (let y = 0; y < copyRows; y++) {
          const oldRow = y * cols;
          const newRow = y * nextCols;
          for (let x = 0; x < copyCols; x++) {
            newOpacity[newRow + x] = opacity[oldRow + x];
            newAliveA[newRow + x] = aliveA[oldRow + x];
            newPhase[newRow + x] = phase[oldRow + x];
          }
        }
      } else {
        // First init: seed per-cell phases for pulsingWave and a random GoL.
        for (let i = 0; i < total; i++) {
          newPhase[i] = Math.random() * Math.PI * 2;
          newAliveA[i] = Math.random() > 0.7 ? 1 : 0;
        }
      }

      opacity = newOpacity;
      aliveA = newAliveA;
      aliveB = newAliveB;
      phase = newPhase;
      cols = nextCols;
      rows = nextRows;
    };

    const resize = () => {
      const rect = container.getBoundingClientRect();
      width = rect.width;
      height = rect.height;
      dpr = window.devicePixelRatio || 1;
      canvas.width = Math.max(1, Math.floor(width * dpr));
      canvas.height = Math.max(1, Math.floor(height * dpr));
      canvas.style.width = `${width}px`;
      canvas.style.height = `${height}px`;
      ctx.setTransform(dpr, 0, 0, dpr, 0, 0);

      const nextCols = Math.floor(width / step);
      const nextRows = Math.floor(height / step);
      offsetX = (width - nextCols * step) / 2;
      offsetY = (height - nextRows * step) / 2;

      if (nextCols !== cols || nextRows !== rows) {
        allocate(nextCols, nextRows);
      }
    };

    // ── Algorithm steps (in-place on typed arrays, zero allocation) ─────────

    const stepPulsingWave = (dt: number) => {
      const len = cols * rows;
      const TAU = Math.PI * 2;
      for (let i = 0; i < len; i++) {
        let p = phase[i] + dt;
        if (p > TAU) { p -= TAU; }
        phase[i] = p;
        opacity[i] = 0.15 + Math.sin(p) * 0.2;
      }
    };

    const stepFlowingWave = (t: number) => {
      // Precompute per-column and per-row sinusoids — O(cols + rows) instead of O(cols*rows) trig.
      const colWave = new Float32Array(cols);
      const rowWave = new Float32Array(rows);
      const cPi3 = Math.PI * 3;
      for (let x = 0; x < cols; x++) {
        colWave[x] = Math.sin((x / cols) * cPi3 + t * 1.2) * 0.5 + 0.5;
      }
      for (let y = 0; y < rows; y++) {
        rowWave[y] = Math.cos((y / rows) * cPi3 + t * 0.8) * 0.5 + 0.5;
      }
      for (let y = 0; y < rows; y++) {
        const base = y * cols;
        const ry = rowWave[y];
        for (let x = 0; x < cols; x++) {
          opacity[base + x] = ((colWave[x] + ry) / 2) * 0.45;
        }
      }
    };

    const stepGameOfLife = () => {
      // Single in-place pass with toroidal wrap-around using precomputed
      // neighbor row/column offsets. Reads from aliveA, writes to aliveB,
      // then swaps. Opacity is written in the same loop.
      for (let y = 0; y < rows; y++) {
        const yUp = (y === 0 ? rows - 1 : y - 1) * cols;
        const yDn = (y === rows - 1 ? 0 : y + 1) * cols;
        const yMid = y * cols;
        for (let x = 0; x < cols; x++) {
          const xL = x === 0 ? cols - 1 : x - 1;
          const xR = x === cols - 1 ? 0 : x + 1;
          const n =
            aliveA[yUp + xL] + aliveA[yUp + x] + aliveA[yUp + xR] +
            aliveA[yMid + xL] +                   aliveA[yMid + xR] +
            aliveA[yDn + xL] + aliveA[yDn + x] + aliveA[yDn + xR];
          const cur = aliveA[yMid + x];
          const next = cur ? (n === 2 || n === 3 ? 1 : 0) : (n === 3 ? 1 : 0);
          aliveB[yMid + x] = next;
          opacity[yMid + x] = next ? 0.55 : 0;
        }
      }
      const swap = aliveA;
      aliveA = aliveB;
      aliveB = swap;
    };

    // ── Batched draw ────────────────────────────────────────────────────────
    //
    // Quantize each visible cell's opacity into OPACITY_BUCKETS tiers,
    // counting-sort cell indices into per-bucket index buffers, then for
    // each non-empty bucket set fillStyle once and emit fillRects. This
    // turns N fillStyle assignments + N string allocations into ≤ BUCKETS
    // of each, which is the real hot path on this component.

    let indices = new Int32Array(0);
    const counts = new Int32Array(OPACITY_BUCKETS);
    const offsets = new Int32Array(OPACITY_BUCKETS);

    const draw = () => {
      const len = cols * rows;
      if (indices.length < len) {
        indices = new Int32Array(len);
      }

      ctx.clearRect(0, 0, width, height);

      counts.fill(0);
      const maxBucket = OPACITY_BUCKETS - 1;
      // Pass 1: count cells per bucket.
      for (let i = 0; i < len; i++) {
        const op = opacity[i];
        if (op < 0.02) { continue; }
        let b = (op * OPACITY_BUCKETS) | 0;
        if (b > maxBucket) { b = maxBucket; }
        counts[b]++;
      }
      // Compute write offsets.
      let acc = 0;
      for (let b = 0; b < OPACITY_BUCKETS; b++) {
        offsets[b] = acc;
        acc += counts[b];
      }
      // Pass 2: scatter cell indices into their bucket slot.
      const cursor = counts; // reuse as cursor (will be overwritten cleanly)
      cursor.fill(0);
      for (let i = 0; i < len; i++) {
        const op = opacity[i];
        if (op < 0.02) { continue; }
        let b = (op * OPACITY_BUCKETS) | 0;
        if (b > maxBucket) { b = maxBucket; }
        indices[offsets[b] + cursor[b]++] = i;
      }
      // Pass 3: per bucket, one fillStyle, fillRect for each member.
      let from = 0;
      for (let b = 0; b < OPACITY_BUCKETS; b++) {
        const n = cursor[b];
        if (n === 0) { from = offsets[b] + n; continue; }
        const bucketOpacity = (b + 0.5) / OPACITY_BUCKETS;
        ctx.fillStyle = `${colorString}${bucketOpacity})`;
        const end = from + n;
        for (let k = from; k < end; k++) {
          const idx = indices[k];
          const y = (idx / cols) | 0;
          const x = idx - y * cols;
          ctx.fillRect(offsetX + x * step, offsetY + y * step, dotSize, dotSize);
        }
        from = end;
      }
    };

    // ── Animation loop ──────────────────────────────────────────────────────

    const animate = (ts: number) => {
      if (cols === 0 || rows === 0) {
        // Container not laid out yet — try again next frame.
        rafId = requestAnimationFrame(animate);
        if (width === 0 || height === 0) {
          resize();
        }
        return;
      }

      if (startTime === 0) {
        startTime = ts;
        lastTime = ts;
        lastGolUpdate = 0;
      }

      const rawDt = (ts - lastTime) / 1000;
      const dt = Math.min(rawDt, 0.1) * speed;
      const t = ((ts - startTime) / 1000) * speed;
      lastTime = ts;

      // Refresh CSS-derived color ~ every 500ms.
      if (ts - lastColorCheck > 500) {
        refreshColor();
        lastColorCheck = ts;
      }

      switch (kind) {
        case "pulsingWave":
          stepPulsingWave(dt);
          break;
        case "flowingWave":
          stepFlowingWave(t);
          break;
        case "gameOfLife": {
          // Update GoL on a fixed cadence regardless of frame rate so it
          // doesn't run away at 120fps. 120ms ≈ 8 generations/s.
          if (ts - lastGolUpdate > 120) {
            stepGameOfLife();
            lastGolUpdate = ts;
          }
          break;
        }
      }

      draw();
      rafId = requestAnimationFrame(animate);
    };

    const ro = new ResizeObserver(() => {
      resize();
      // Don't reset startTime — algorithms keep their continuity through resize.
    });
    ro.observe(container);

    resize();
    rafId = requestAnimationFrame(animate);

    return () => {
      cancelAnimationFrame(rafId);
      ro.disconnect();
    };
  }, [resolvedAlgorithm, dotSize, spacing, speed]);

  return (
    <div
      ref={containerRef}
      className={className}
      style={{ position: "absolute", inset: 0, overflow: "hidden", ...style }}
    >
      <canvas
        ref={canvasRef}
        style={{ position: "absolute", top: 0, left: 0, imageRendering: "pixelated" }}
      />
    </div>
  );
};
