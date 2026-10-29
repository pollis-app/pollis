import React, { useEffect, useMemo, useRef } from "react";
import { hslToHex } from "../../utils/colorUtils";

// Algorithms are tagged objects, not functions: the renderer dispatches on
// `kind` to typed-array loops that never allocate.

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

interface DotMatrixProps {
  algorithm?: DotMatrixAlgorithm;
  dotSize?: number;
  spacing?: number;
  speed?: number;
  className?: string;
  style?: React.CSSProperties;
}

// The canvas holds ONE PIXEL PER CELL: each frame writes intensities into a
// Uint32 ImageData view and does one putImageData. The compositor scales it up
// with nearest-neighbour and a CSS mask cuts the cells into dots.

// Below this intensity a cell is drawn fully transparent.
const MIN_VISIBLE_OPACITY = 0.02;

// Revive a few cells every N generations so the board never settles.
const GOL_RESEED_EVERY = 30;
const GOL_RESEED_CHANCE = 0.03;

const parseAccentRgb = (): number => {
  const cs = getComputedStyle(document.documentElement);
  const rawH = parseFloat(cs.getPropertyValue("--accent-h"));
  const rawS = parseFloat(cs.getPropertyValue("--accent-s"));
  // A hue or saturation of 0 is valid, so only fall back when unparseable.
  const h = Number.isNaN(rawH) ? 38 : rawH;
  const s = Number.isNaN(rawS) ? 90 : rawS;
  const hex = hslToHex(h, s, 62);
  const r = parseInt(hex.slice(1, 3), 16);
  const g = parseInt(hex.slice(3, 5), 16);
  const b = parseInt(hex.slice(5, 7), 16);
  // ImageData is RGBA in memory, which is ABGR when read as a little-endian u32.
  return (b << 16) | (g << 8) | r;
};

const dotMaskUrl = (dotSize: number, step: number): string =>
  `url("data:image/svg+xml,${encodeURIComponent(
    `<svg xmlns='http://www.w3.org/2000/svg' width='${step}' height='${step}'><rect width='${dotSize}' height='${dotSize}'/></svg>`
  )}")`;

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

    // Shift the canvas back half a gap so cell edges fall in the masked gaps,
    // hiding nearest-neighbour rounding at fractional DPR.
    const mask = dotMaskUrl(dotSize, step);
    const half = spacing / 2;
    const cs = canvas.style;
    cs.setProperty("-webkit-mask-image", mask);
    cs.setProperty("mask-image", mask);
    cs.setProperty("-webkit-mask-size", `${step}px ${step}px`);
    cs.setProperty("mask-size", `${step}px ${step}px`);
    cs.setProperty("-webkit-mask-position", `${half}px ${half}px`);
    cs.setProperty("mask-position", `${half}px ${half}px`);

    let cols = 0;
    let rows = 0;

    // 0..1 per cell, row-major
    let opacity = new Float32Array(0);
    // GoL alive/dead, double-buffered
    let aliveA = new Uint8Array(0);
    let aliveB = new Uint8Array(0);
    let phase = new Float32Array(0);
    let image: ImageData | null = null;
    let pixels = new Uint32Array(0);

    let startTime = 0;
    let lastTime = 0;
    let lastGolUpdate = 0;
    let generation = 0;
    let dirty = true;
    let rafId = 0;
    let rgb = parseAccentRgb();

    // The accent is inline custom properties on <html>; observe that attribute.
    const colorObserver = new MutationObserver(() => {
      rgb = parseAccentRgb();
      dirty = true;
    });
    colorObserver.observe(document.documentElement, { attributes: true, attributeFilter: ["style", "class"] });

    const allocate = (nextCols: number, nextRows: number) => {
      const total = nextCols * nextRows;
      const newOpacity = new Float32Array(total);
      const newAliveA = new Uint8Array(total);
      const newAliveB = new Uint8Array(total);
      const newPhase = new Float32Array(total);

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
        for (let i = 0; i < total; i++) {
          newPhase[i] = Math.random() * Math.PI * 2;
          newAliveA[i] = Math.random() > 0.7 ? 1 : 0;
          newOpacity[i] = newAliveA[i] ? 0.55 : 0;
        }
      }

      opacity = newOpacity;
      aliveA = newAliveA;
      aliveB = newAliveB;
      phase = newPhase;
      cols = nextCols;
      rows = nextRows;

    // Resizing a canvas clears it, so only when the grid changes.
      canvas.width = Math.max(1, cols);
      canvas.height = Math.max(1, rows);
      image = total > 0 ? ctx.createImageData(cols, rows) : null;
      pixels = image ? new Uint32Array(image.data.buffer) : new Uint32Array(0);
    };

    const resize = () => {
      const rect = container.getBoundingClientRect();
      const nextCols = Math.floor(rect.width / step);
      const nextRows = Math.floor(rect.height / step);
      const offsetX = (rect.width - nextCols * step) / 2;
      const offsetY = (rect.height - nextRows * step) / 2;

      cs.left = `${offsetX - half}px`;
      cs.top = `${offsetY - half}px`;
      cs.width = `${nextCols * step}px`;
      cs.height = `${nextRows * step}px`;

      if (nextCols !== cols || nextRows !== rows) {
        allocate(nextCols, nextRows);
        dirty = true;
      }
    };

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

    let colWave = new Float32Array(0);
    let rowWave = new Float32Array(0);

    const stepFlowingWave = (t: number) => {
      if (colWave.length !== cols) { colWave = new Float32Array(cols); }
      if (rowWave.length !== rows) { rowWave = new Float32Array(rows); }
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
      generation++;
      const reseed = generation % GOL_RESEED_EVERY === 0;
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
          let next = cur ? (n === 2 || n === 3 ? 1 : 0) : (n === 3 ? 1 : 0);
          if (reseed && Math.random() < GOL_RESEED_CHANCE) {
            next = 1;
          }
          aliveB[yMid + x] = next;
          opacity[yMid + x] = next ? 0.55 : 0;
        }
      }
      const swap = aliveA;
      aliveA = aliveB;
      aliveB = swap;
    };

    const draw = () => {
      if (!image) { return; }
      const len = cols * rows;
      const color = rgb;
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
    };

    const animate = (ts: number) => {
      rafId = requestAnimationFrame(animate);

      if (cols === 0 || rows === 0) {
        resize();
        return;
      }

      if (startTime === 0) {
        startTime = ts;
        lastTime = ts;
        lastGolUpdate = ts;
      }

      const rawDt = (ts - lastTime) / 1000;
      const dt = Math.min(rawDt, 0.1) * speed;
      const t = ((ts - startTime) / 1000) * speed;
      lastTime = ts;

      switch (kind) {
        case "pulsingWave":
          stepPulsingWave(dt);
          dirty = true;
          break;
        case "flowingWave":
          stepFlowingWave(t);
          dirty = true;
          break;
        case "gameOfLife": {
          // Fixed ~8 generations/s regardless of frame rate.
          if (ts - lastGolUpdate > 120) {
            stepGameOfLife();
            lastGolUpdate = ts;
            dirty = true;
          }
          break;
        }
      }

      if (dirty) {
        draw();
        dirty = false;
      }
    };

    const ro = new ResizeObserver(() => {
      resize();
    });
    ro.observe(container);

    resize();
    rafId = requestAnimationFrame(animate);

    return () => {
      cancelAnimationFrame(rafId);
      ro.disconnect();
      colorObserver.disconnect();
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
        style={{
          position: "absolute",
          top: 0,
          left: 0,
          imageRendering: "pixelated",
          WebkitMaskRepeat: "repeat",
          maskRepeat: "repeat",
        }}
      />
    </div>
  );
};
