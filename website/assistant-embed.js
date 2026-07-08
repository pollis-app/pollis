// MiniLM sentence-embedding for the assistant's semantic tier — extractive, no generative model.
//
// The model (all-MiniLM-L6-v2, int8 ONNX) runs ENTIRELY in the visitor's browser via a VENDORED copy
// of transformers.js + ONNX-Runtime-Web (website/vendor/). There is NO remote/CDN call and NO query
// egress: `env.allowRemoteModels = false` forbids any model fetch, and only static first-party assets
// (the lib, the WASM runtime, the model weights) are loaded — the question text never leaves the device.
//
// transformers.js is imported DYNAMICALLY inside loadExtractor() (not at module top) so this file — and
// assistant.js which imports it — stay importable in Node for logic tests, and so the ~0.5 MB library +
// ~13-23 MB WASM + ~22 MB model are fetched only the first time the panel opens (then cached).

// Pure helpers (no browser/library dependency) — safe to import anywhere, including Node tests.

// Cosine of two equal-length unit vectors (already unit vectors → dot product).
export function cosine(a, b) {
  let s = 0;
  for (let i = 0; i < a.length; i += 1) {
    s += a[i] * b[i];
  }
  return s;
}

// Decode a base64 int8 vector (a precomputed answer/chunk embedding) into a dequantized unit
// Float32Array (stored scale is 1/127).
export function decodeI8Unit(b64, dim) {
  const bin = typeof atob === 'function' ? atob(b64) : Buffer.from(b64, 'base64').toString('binary');
  const v = new Float32Array(dim);
  for (let i = 0; i < dim; i += 1) {
    const byte = bin.charCodeAt(i);
    v[i] = (byte > 127 ? byte - 256 : byte) / 127;
  }
  return v;
}

// Browser-only: lazily import the vendored transformers.js, pin it to fully-offline first-party assets,
// and build the feature-extraction pipeline. Query text never leaves the device.
let extractorPromise = null;
export function loadExtractor() {
  if (extractorPromise) {
    return extractorPromise;
  }
  extractorPromise = (async () => {
    const { pipeline, env } = await import('./vendor/transformers/transformers.min.js');
    // Fully offline, first-party only.
    env.allowRemoteModels = false;
    env.allowLocalModels = true;
    env.localModelPath = new URL('./vendor/models/', import.meta.url).href;
    env.backends.onnx.wasm.wasmPaths = new URL('./vendor/ort/', import.meta.url).href;
    // Single-threaded WASM → no SharedArrayBuffer, so the site needs no cross-origin-isolation headers.
    env.backends.onnx.wasm.numThreads = 1;
    return pipeline('feature-extraction', 'Xenova/all-MiniLM-L6-v2', { quantized: true });
  })();
  return extractorPromise;
}

// Embed one string into a mean-pooled, L2-normalized vector (plain number[] for cosine).
export async function embed(extractor, text) {
  const out = await extractor(text, { pooling: 'mean', normalize: true });
  if (out && out.data) {
    return Array.from(out.data);
  }
  // Fallback for a Tensor-shaped return or an injected test double.
  const list = typeof out.tolist === 'function' ? out.tolist() : out;
  return Array.isArray(list[0]) ? list[0] : list;
}
