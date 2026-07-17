// Remote screen-share / camera renderer.
//
// All real-time media runs in Rust (WebKitGTK on Linux has no WebRTC): the
// backend decodes frames and pushes I420 planes to the renderer over IPC, and
// a WebGL shader does YUV→RGB on a `<canvas>`.

import React, { useEffect, useRef } from "react";

import { screenShareSession, type DecodedFrame } from "../../screenshare/screenShareSession";
import { LOCAL_CAMERA_PREVIEW_KEY } from "../../camera/cameraSession";

interface Props {
  trackKey: string;
  className?: string;
  /** Hint used for canvas backing-store size before the first frame arrives. */
  initialWidth?: number;
  initialHeight?: number;
  /** Low-cost rendering for in-grid previews: cap repaints at ~15fps. The
   *  fullscreen viewer omits this for source-quality rendering. */
  preview?: boolean;
  /** Horizontally flip the rendered image. Used for a self-view (your own
   *  webcam) so it reads like a mirror — never for remote participants. */
  mirror?: boolean;
}

const PREVIEW_MIN_PAINT_INTERVAL_MS = 1000 / 15;

// ── WebGL / I420 renderer ────────────────────────────────────────────────────

/// 2× nearest-neighbour downsample of a single 8-bit plane. We sample
/// every other pixel from every other row into a tightly-packed
/// destination buffer. Allocated once and reused across frames (the
/// caller owns the scratch) so the hot path stays allocation-free.
const VERT_SRC = `
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
  v_uv = vec2((a_pos.x + 1.0) * 0.5, 1.0 - (a_pos.y + 1.0) * 0.5);
  gl_Position = vec4(a_pos, 0.0, 1.0);
}
`;

// BT.601 limited-range YUV→RGB. Same matrix libwebrtc uses.
const FRAG_SRC = `
precision mediump float;
varying vec2 v_uv;
uniform sampler2D u_y;
uniform sampler2D u_u;
uniform sampler2D u_v;
void main() {
  float y = texture2D(u_y, v_uv).r;
  float u = texture2D(u_u, v_uv).r - 0.5;
  float v = texture2D(u_v, v_uv).r - 0.5;
  // BT.601
  float r = y + 1.402 * v;
  float g = y - 0.344136 * u - 0.714136 * v;
  float b = y + 1.772 * u;
  gl_FragColor = vec4(r, g, b, 1.0);
}
`;

function compile(gl: WebGLRenderingContext, type: number, src: string): WebGLShader {
  const sh = gl.createShader(type)!;
  gl.shaderSource(sh, src);
  gl.compileShader(sh);
  if (!gl.getShaderParameter(sh, gl.COMPILE_STATUS)) {
    const log = gl.getShaderInfoLog(sh);
    gl.deleteShader(sh);
    throw new Error(`shader compile: ${log}`);
  }
  return sh;
}

interface GLBundle {
  gl: WebGLRenderingContext;
  prog: WebGLProgram;
  texY: WebGLTexture;
  texU: WebGLTexture;
  texV: WebGLTexture;
  // Currently-allocated texture sizes per plane. We keep these to call
  // texImage2D (allocate) only when a plane size changes; otherwise we
  // texSubImage2D (upload only) which avoids reallocating GPU memory each
  // frame. Net effect: ~zero allocation per frame after warmup.
  yW: number; yH: number;
  uW: number; uH: number;
  vW: number; vH: number;
}

function initGL(canvas: HTMLCanvasElement): GLBundle | null {
  const ctx = canvas.getContext("webgl", { antialias: false, alpha: false, premultipliedAlpha: false }) as WebGLRenderingContext | null;
  if (!ctx) {
    return null;
  }
  const gl: WebGLRenderingContext = ctx;
  const vs = compile(gl, gl.VERTEX_SHADER, VERT_SRC);
  const fs = compile(gl, gl.FRAGMENT_SHADER, FRAG_SRC);
  const prog = gl.createProgram()!;
  gl.attachShader(prog, vs);
  gl.attachShader(prog, fs);
  gl.linkProgram(prog);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
    const log = gl.getProgramInfoLog(prog);
    throw new Error(`program link: ${log}`);
  }
  gl.useProgram(prog);

  // Full-screen triangle pair.
  const buf = gl.createBuffer();
  gl.bindBuffer(gl.ARRAY_BUFFER, buf);
  gl.bufferData(
    gl.ARRAY_BUFFER,
    new Float32Array([-1, -1, 1, -1, -1, 1, -1, 1, 1, -1, 1, 1]),
    gl.STATIC_DRAW
  );
  const aPos = gl.getAttribLocation(prog, "a_pos");
  gl.enableVertexAttribArray(aPos);
  gl.vertexAttribPointer(aPos, 2, gl.FLOAT, false, 0, 0);

  function newTex(unit: number, uniform: string): WebGLTexture {
    const tex = gl.createTexture()!;
    gl.activeTexture(gl.TEXTURE0 + unit);
    gl.bindTexture(gl.TEXTURE_2D, tex);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.LINEAR);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE);
    gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE);
    gl.uniform1i(gl.getUniformLocation(prog, uniform), unit);
    return tex;
  }
  const texY = newTex(0, "u_y");
  const texU = newTex(1, "u_u");
  const texV = newTex(2, "u_v");
  gl.pixelStorei(gl.UNPACK_ALIGNMENT, 1);

  return {
    gl, prog, texY, texU, texV,
    yW: 0, yH: 0, uW: 0, uH: 0, vW: 0, vH: 0,
  };
}

function uploadPlane(
  gl: WebGLRenderingContext,
  unit: number,
  tex: WebGLTexture,
  data: Uint8Array,
  width: number,
  height: number,
  alloc: { w: number; h: number }
) {
  gl.activeTexture(gl.TEXTURE0 + unit);
  gl.bindTexture(gl.TEXTURE_2D, tex);
  if (alloc.w !== width || alloc.h !== height) {
    gl.texImage2D(
      gl.TEXTURE_2D, 0, gl.LUMINANCE, width, height, 0,
      gl.LUMINANCE, gl.UNSIGNED_BYTE, data
    );
    alloc.w = width;
    alloc.h = height;
  } else {
    gl.texSubImage2D(
      gl.TEXTURE_2D, 0, 0, 0, width, height,
      gl.LUMINANCE, gl.UNSIGNED_BYTE, data
    );
  }
}

const RemoteVideoTileImpl: React.FC<Props> = ({
  trackKey,
  className,
  initialWidth,
  initialHeight,
  preview = false,
  mirror,
}) => {
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const glRef = useRef<GLBundle | null>(null);
  // Decoupling: the frame callback fires off the message-port thread; we
  // throttle to rAF so paint cost is bounded by display refresh and we
  // drop intermediate frames cleanly.
  const pendingRef = useRef<DecodedFrame | null>(null);
  const rafRef = useRef<number | null>(null);
  // Preview-mode 15fps gate. We compare against the last paint and
  // skip scheduling the rAF when a frame arrives too soon — the
  // upcoming frame will check again.
  const lastPaintAtRef = useRef<number>(0);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }
    if (initialWidth && initialHeight) {
      canvas.width = initialWidth;
      canvas.height = initialHeight;
    }
    glRef.current = initGL(canvas);
    if (!glRef.current) {
      console.error("[RemoteVideoTile] WebGL init failed");
    }

    const render = () => {
      rafRef.current = null;
      lastPaintAtRef.current = performance.now();
      const frame = pendingRef.current;
      pendingRef.current = null;
      if (!frame || !glRef.current) {
        return;
      }
      const bundle = glRef.current;
      const { gl } = bundle;
      const cW = (frame.width + 1) >> 1;
      const cH = (frame.height + 1) >> 1;
      // Strides may exceed width when the source row is padded; we'd need
      // per-row uploads to handle that correctly. PipeWire BGRx + libyuv
      // I420 typically returns tightly packed planes for our resolutions —
      // assert and bail if not, rather than silently render garbage.
      if (frame.yStride !== frame.width || frame.uStride !== cW || frame.vStride !== cW) {
        return;
      }
      // Upload the I420 planes at source resolution and let WebGL's LINEAR
      // filter + the canvas's CSS box scale the thumbnail down. The earlier
      // CPU 2× downsample (preview mode) is gone: it mis-positioned the tile
      // image while fullscreen was correct, and the rustwebrtc PoC showed GPU
      // paint is ~1ms p95 even at 1440p — there's no bandwidth win worth a
      // separate, buggy code path. One path now renders both tile + fullscreen.
      const yPlane = frame.y;
      const uPlane = frame.u;
      const vPlane = frame.v;
      const yW = frame.width;
      const yH = frame.height;
      const uvW = cW;
      const uvH = cH;
      // Resize backing store on dimension change. CSS sizing is
      // independent — the canvas's intrinsic dimensions drive
      // max-width:100%+max-height:100%+width:auto+height:auto sizing.
      if (canvas.width !== yW || canvas.height !== yH) {
        canvas.width = yW;
        canvas.height = yH;
      }
      const yAlloc = { w: bundle.yW, h: bundle.yH };
      const uAlloc = { w: bundle.uW, h: bundle.uH };
      const vAlloc = { w: bundle.vW, h: bundle.vH };
      uploadPlane(gl, 0, bundle.texY, yPlane, yW, yH, yAlloc);
      uploadPlane(gl, 1, bundle.texU, uPlane, uvW, uvH, uAlloc);
      uploadPlane(gl, 2, bundle.texV, vPlane, uvW, uvH, vAlloc);
      bundle.yW = yAlloc.w; bundle.yH = yAlloc.h;
      bundle.uW = uAlloc.w; bundle.uH = uAlloc.h;
      bundle.vW = vAlloc.w; bundle.vH = vAlloc.h;
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    };

    const unsubscribe = screenShareSession.onFrame(trackKey, (frame) => {
      pendingRef.current = frame;
      // Preview-mode 15fps cap: if we painted recently, skip
      // scheduling the rAF. The next frame arrival will check again
      // and schedule once the interval has elapsed.
      if (preview) {
        const elapsed = performance.now() - lastPaintAtRef.current;
        if (elapsed < PREVIEW_MIN_PAINT_INTERVAL_MS) {
          return;
        }
      }
      if (rafRef.current === null) {
        rafRef.current = requestAnimationFrame(render);
      }
    });

    return () => {
      unsubscribe();
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      pendingRef.current = null;
      // GL resources are tied to the canvas; dropping the canvas releases
      // them when the WebGL context is garbage collected.
      glRef.current = null;
    };
  }, [trackKey, initialWidth, initialHeight, preview]);

  // Parent's flex layout (justify/align center) does the centering;
  // the canvas auto-sizes from its intrinsic dimensions (the width/
  // height attributes we sync to the source resolution in render()),
  // and max-width/max-height clamp it to fit the parent in both axes
  // while the auto/auto width/height keeps the aspect ratio.
  return (
    <canvas
      ref={canvasRef}
      data-testid={`remote-video-tile-${trackKey}`}
      className={className}
      style={{
        display: "block",
        maxWidth: "100%",
        maxHeight: "100%",
        width: "auto",
        height: "auto",
        background: "#000",
        transform: mirror ? "scaleX(-1)" : undefined,
      }}
    />
  );
};

// ── Dispatch ────────────────────────────────────────────────────────────────

export const RemoteVideoTile: React.FC<Props> = (props) => {
  // Auto-mirror the local camera self-view (your own webcam) so it reads like a
  // mirror — both the in-call self-preview and the settings preview render
  // through here under LOCAL_CAMERA_PREVIEW_KEY. Explicit `mirror` still wins.
  const mirror = props.mirror ?? props.trackKey === LOCAL_CAMERA_PREVIEW_KEY;
  return <RemoteVideoTileImpl {...props} mirror={mirror} />;
};
