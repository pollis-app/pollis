# Assistant vendored runtime (issue #508)

These assets power the site assistant's optional **semantic** tier (see `../assistant.js`,
`../assistant-embed.js`). They are vendored — not fetched from a CDN — so the feature is fully
self-contained and makes **no third-party network call**; a visitor's question never leaves the
device. Everything here is loaded **lazily** the first time the assistant panel opens, then cached.

## Contents

| Path | What | Size | License |
|---|---|---|---|
| `transformers/transformers.min.js` | transformers.js **4.2.0**, self-contained browser build | ~0.5 MB | Apache-2.0 (Hugging Face) |
| `ort/ort-wasm-simd-threaded.{wasm,mjs}` | ONNX-Runtime-Web CPU (SIMD) | ~13 MB | MIT (Microsoft) |
| `ort/ort-wasm-simd-threaded.asyncify.{wasm,mjs}` | ONNX-Runtime-Web CPU (single-thread asyncify) | ~23 MB | MIT (Microsoft) |
| `models/Xenova/all-MiniLM-L6-v2/` | all-MiniLM-L6-v2 sentence embedder, int8 ONNX + tokenizer | ~22 MB | Apache-2.0 |

Runtime config (`assistant-embed.js`): `env.allowRemoteModels = false` (never fetch a model),
`localModelPath`/`wasmPaths` point here, `wasm.numThreads = 1` (no SharedArrayBuffer → the site needs
no cross-origin-isolation headers). The model is an **extractive** re-ranker: it only helps choose
which doc-exact canned answer or cited excerpt to surface. There is no generative model.

## Regenerating the precomputed vectors (`../assistant-vectors.json`)

The canned-answer and corpus-chunk embeddings are precomputed with the **same** engine used at runtime
(transformers.js) so build-time and query-time vectors align. With Node + `@huggingface/transformers`:

```js
import { pipeline, env } from '@huggingface/transformers';
env.allowRemoteModels = false;
env.localModelPath = 'website/vendor/models/';
const ext = await pipeline('feature-extraction', 'Xenova/all-MiniLM-L6-v2', { quantized: true });
const emb = async (t) => (await ext(t, { pooling: 'mean', normalize: true })).data; // Float32
// For each answer: emb(question + ' ' + answer); for each chunk: emb(title + ' ' + text)
// Quantize each to int8 (round(x*127)) and base64-encode into assistant-vectors.json {dim:384,...}.
```

Regenerate whenever `assistant-answers.json` or `assistant-corpus.json` change, or the answers/excerpts
will be retrieved against stale vectors.
