// Remote video: Rust decodes frames and pushes I420 planes over the loopback frame socket;
// WebGL converts them on a canvas. The tile reports what it needs back to Rust (`videoWants`).

import React, { useEffect, useRef } from "react";

import { screenShareSession, type DecodedFrame } from "../../screenshare/screenShareSession";
import { videoWants } from "../../screenshare/videoWants";
import { LOCAL_CAMERA_PREVIEW_KEY } from "../../camera/cameraSession";

interface Props {
  trackKey: string;
  className?: string;
  initialWidth?: number;
  initialHeight?: number;
  /** In-grid previews: cap repaints at ~15fps. */
  preview?: boolean;
  /** Flip horizontally; self-view only. */
  mirror?: boolean;
}

const PREVIEW_MIN_PAINT_INTERVAL_MS = 1000 / 15;
const PREVIEW_WANT_FPS = 15;
// A ceiling, not a target: the publisher's own cap is lower; "as fast as it arrives".
const FULL_WANT_FPS = 60;
// The backing store never exceeds this many device pixels per CSS pixel: past 2× the extra
// texels only cost upload and GPU time, they cannot show.
const MAX_BACKING_SCALE = 2;

const VERT_SRC = `
attribute vec2 a_pos;
varying vec2 v_uv;
void main() {
  v_uv = vec2((a_pos.x + 1.0) * 0.5, 1.0 - (a_pos.y + 1.0) * 0.5);
  gl_Position = vec4(a_pos, 0.0, 1.0);
}
`;

// BT.601 limited-range, as libwebrtc.
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
  const sh = gl.createShader(type);
  if (!sh) {
    // A context that is already lost hands out nulls; see `initGL`'s caller.
    throw new Error("shader create: context lost");
  }
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
  vbo: WebGLBuffer;
  texY: WebGLTexture;
  texU: WebGLTexture;
  texV: WebGLTexture;
  // Allocated sizes per plane: texImage2D only on change, else texSubImage2D.
  yW: number; yH: number;
  uW: number; uH: number;
  vW: number; vH: number;
  // Scratch for repacking a padded plane into a tight one; grown on demand.
  scratch: Uint8Array;
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
  // Linked into the program; the shader objects themselves are no longer needed.
  gl.deleteShader(vs);
  gl.deleteShader(fs);
  if (!gl.getProgramParameter(prog, gl.LINK_STATUS)) {
    const log = gl.getProgramInfoLog(prog);
    gl.deleteProgram(prog);
    throw new Error(`program link: ${log}`);
  }
  gl.useProgram(prog);

  const vbo = gl.createBuffer()!;
  gl.bindBuffer(gl.ARRAY_BUFFER, vbo);
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
    gl, prog, vbo, texY, texU, texV,
    yW: 0, yH: 0, uW: 0, uH: 0, vW: 0, vH: 0,
    scratch: new Uint8Array(0),
  };
}

/** Release every GPU object; a canvas that merely unmounts keeps its textures alive until
 *  the GC finds it. The context itself is left to the GC: `WEBGL_lose_context.loseContext()`
 *  on WebKitGTK (software GL, Xvfb) left every context created afterwards lost at birth
 *  (`createShader` → null), which took the whole renderer down at the next tile. */
function destroyGL(bundle: GLBundle) {
  const { gl } = bundle;
  gl.deleteTexture(bundle.texY);
  gl.deleteTexture(bundle.texU);
  gl.deleteTexture(bundle.texV);
  gl.deleteBuffer(bundle.vbo);
  gl.deleteProgram(bundle.prog);
}

/** The plane as a tight `width × height` block: the data itself when its stride already is,
 *  else a row-by-row repack into the bundle's scratch (WebGL1 has no UNPACK_ROW_LENGTH). */
function tight(
  bundle: GLBundle,
  data: Uint8Array,
  width: number,
  height: number,
  stride: number,
): Uint8Array {
  if (stride === width) {
    return data;
  }
  const need = width * height;
  if (bundle.scratch.length < need) {
    bundle.scratch = new Uint8Array(need);
  }
  const out = bundle.scratch.subarray(0, need);
  for (let row = 0; row < height; row++) {
    out.set(data.subarray(row * stride, row * stride + width), row * width);
  }
  return out;
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
  // Throttle to rAF; intermediate frames are dropped.
  const pendingRef = useRef<DecodedFrame | null>(null);
  const rafRef = useRef<number | null>(null);
  const trailingRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const lastPaintAtRef = useRef<number>(0);
  // The container's box in device pixels, from the wants observer: the backing store
  // follows it (≤ 2× CSS size) rather than the source.
  const boxPxRef = useRef<{ w: number; h: number }>({ w: 0, h: 0 });

  // One GL context per canvas lifetime: textures and program are allocated once and freed
  // (with the context) on unmount, never re-created when a prop changes.
  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }
    if (initialWidth && initialHeight) {
      // Before the first frame, size the box from what the publisher announced.
      canvas.width = initialWidth;
      canvas.height = initialHeight;
    }
    // A tile that cannot get a working context stays black; it must never throw into the
    // error boundary and take the call UI with it.
    try {
      glRef.current = initGL(canvas);
    } catch (e) {
      glRef.current = null;
      console.error("[RemoteVideoTile] WebGL init failed", e);
    }
    if (!glRef.current) {
      console.error("[RemoteVideoTile] no WebGL context for", trackKey);
    }
    return () => {
      if (glRef.current) {
        destroyGL(glRef.current);
        glRef.current = null;
      }
    };
    // Mount-only on purpose: the announced size only seeds the box before the first frame,
    // and a later change must not tear the context down.
  }, []);

  // Report what this tile needs — its container's size in device pixels while it is on screen,
  // nothing while hidden — so Rust can pick the SFU layer, pause the track and pack to size.
  useEffect(() => {
    const canvas = canvasRef.current;
    const box = canvas?.parentElement;
    if (!canvas || !box) {
      return;
    }
    const id = videoWants.register(trackKey);
    let boxW = box.clientWidth;
    let boxH = box.clientHeight;
    let intersecting = true;
    const report = () => {
      const dpr = window.devicePixelRatio || 1;
      boxPxRef.current = { w: Math.ceil(boxW * dpr), h: Math.ceil(boxH * dpr) };
      const shown = intersecting && document.visibilityState !== "hidden" && boxW > 0 && boxH > 0;
      if (!shown) {
        videoWants.report(trackKey, id, null);
        return;
      }
      videoWants.report(trackKey, id, {
        width: boxPxRef.current.w,
        height: boxPxRef.current.h,
        fps: preview ? PREVIEW_WANT_FPS : FULL_WANT_FPS,
      });
    };
    const resize = new ResizeObserver((entries) => {
      const rect = entries[0]?.contentRect;
      if (rect) {
        boxW = rect.width;
        boxH = rect.height;
      }
      report();
    });
    resize.observe(box);
    const intersection = new IntersectionObserver((entries) => {
      intersecting = entries.some((e) => e.isIntersecting);
      report();
    });
    intersection.observe(canvas);
    document.addEventListener("visibilitychange", report);
    report();
    return () => {
      document.removeEventListener("visibilitychange", report);
      intersection.disconnect();
      resize.disconnect();
      videoWants.unregister(trackKey, id);
    };
  }, [trackKey, preview]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) {
      return;
    }

    const render = () => {
      rafRef.current = null;
      lastPaintAtRef.current = performance.now();
      const frame = pendingRef.current;
      pendingRef.current = null;
      const bundle = glRef.current;
      if (!frame || !bundle) {
        return;
      }
      const { gl } = bundle;
      const cW = (frame.width + 1) >> 1;
      const cH = (frame.height + 1) >> 1;
      // Upload at source resolution; the GPU scales to the box. Padded strides (an AV1 or
      // hardware decoder) are repacked tight.
      const yPlane = tight(bundle, frame.y, frame.width, frame.height, frame.yStride);
      const uPlane = tight(bundle, frame.u, cW, cH, frame.uStride);
      const vPlane = tight(bundle, frame.v, cW, cH, frame.vStride);
      // Backing store: the source, capped at MAX_BACKING_SCALE × the box. It never drops
      // below the box, so the auto-sized canvas still fills its container.
      const box = boxPxRef.current;
      let bw = frame.width;
      let bh = frame.height;
      if (box.w > 0 && box.h > 0) {
        const cap = Math.min(1, (box.w * MAX_BACKING_SCALE) / frame.width, (box.h * MAX_BACKING_SCALE) / frame.height);
        if (cap < 1) {
          bw = Math.max(2, Math.round(frame.width * cap));
          bh = Math.max(2, Math.round(frame.height * cap));
        }
      }
      if (canvas.width !== bw || canvas.height !== bh) {
        canvas.width = bw;
        canvas.height = bh;
      }
      const yAlloc = { w: bundle.yW, h: bundle.yH };
      const uAlloc = { w: bundle.uW, h: bundle.uH };
      const vAlloc = { w: bundle.vW, h: bundle.vH };
      uploadPlane(gl, 0, bundle.texY, yPlane, frame.width, frame.height, yAlloc);
      uploadPlane(gl, 1, bundle.texU, uPlane, cW, cH, uAlloc);
      uploadPlane(gl, 2, bundle.texV, vPlane, cW, cH, vAlloc);
      bundle.yW = yAlloc.w; bundle.yH = yAlloc.h;
      bundle.uW = uAlloc.w; bundle.uH = uAlloc.h;
      bundle.vW = vAlloc.w; bundle.vH = vAlloc.h;
      gl.viewport(0, 0, canvas.width, canvas.height);
      gl.drawArrays(gl.TRIANGLES, 0, 6);
    };

    const schedule = () => {
      if (rafRef.current === null) {
        rafRef.current = requestAnimationFrame(render);
      }
    };

    const unsubscribe = screenShareSession.onFrame(trackKey, (frame) => {
      pendingRef.current = frame;
      if (preview) {
        const elapsed = performance.now() - lastPaintAtRef.current;
        if (elapsed < PREVIEW_MIN_PAINT_INTERVAL_MS) {
          // Inside the cap: a trailing paint, so the last frame of a burst still shows.
          if (trailingRef.current === null) {
            trailingRef.current = setTimeout(() => {
              trailingRef.current = null;
              if (pendingRef.current) {
                schedule();
              }
            }, PREVIEW_MIN_PAINT_INTERVAL_MS - elapsed);
          }
          return;
        }
      }
      schedule();
    });

    return () => {
      unsubscribe();
      if (rafRef.current !== null) {
        cancelAnimationFrame(rafRef.current);
        rafRef.current = null;
      }
      if (trailingRef.current !== null) {
        clearTimeout(trailingRef.current);
        trailingRef.current = null;
      }
      pendingRef.current = null;
    };
  }, [trackKey, preview]);

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

export const RemoteVideoTile: React.FC<Props> = (props) => {
  // Auto-mirror the local camera self-view; explicit `mirror` wins.
  const mirror = props.mirror ?? props.trackKey === LOCAL_CAMERA_PREVIEW_KEY;
  return <RemoteVideoTileImpl {...props} mirror={mirror} />;
};
