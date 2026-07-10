// Pollis "Ask about how this works" assistant — retrieval + curated canned answers, with an
// optional tiny SEMANTIC re-ranker (Phase 1, extractive — still no generative model).
//
// The always-on floor runs ENTIRELY from static JSON shipped with the page: no LLM, no generative
// model, no WebGPU/WASM runtime, no network calls at query time. Answers come from two places:
//   1. a curated canned-answer map (assistant-answers.json) — the doc-faithful, hand-checked path;
//   2. a lexical search over a chunked corpus (assistant-corpus.json), returning source excerpts.
// If neither is confident, it says it does not know and points at the closest page. It NEVER
// synthesizes prose of its own — it only surfaces text a human authored and cited.
//
// The optional semantic tier improves *which* answer/excerpt is surfaced for paraphrased questions.
// It is the all-MiniLM-L6-v2 sentence embedder (int8 ONNX) running ENTIRELY in the browser via a
// VENDORED transformers.js + ONNX-Runtime-Web (assistant-embed.js) — an EXTRACTIVE re-ranker, NOT a
// generative model. It is loaded LAZILY the first time the panel opens, from same-origin static assets
// only (never a remote/CDN, and the query text never leaves the device). Until it loads (or if it
// fails), behaviour is exactly the lexical floor. Semantic scores are FUSED with lexical scores, and a
// paraphrase can be routed to a doc-exact canned answer only above a validated high-precision cosine
// floor. The result stays fully extractive and cited.

// Load the static data at runtime rather than via `import ... with { type: 'json' }`:
// JSON-module import attributes only became cross-browser Baseline in mid-2025, and an
// unsupported engine turns that import into a parse error that kills the ENTIRE module —
// defeating the always-on lexical floor. A same-origin fetch (top-level await, Baseline
// since 2021) loads the identical files on every browser. Resolved against import.meta.url
// so the path is correct regardless of the page's URL; a failed fetch degrades to empty
// data (the widget still renders and simply abstains) instead of throwing.
const loadJSON = async (rel) => {
  try {
    const res = await fetch(new URL(rel, import.meta.url));
    return res.ok ? await res.json() : {};
  } catch {
    return {};
  }
};
const [corpusDoc, answersDoc, vectorsDoc] = await Promise.all([
  loadJSON('./assistant-corpus.json'),
  loadJSON('./assistant-answers.json'),
  loadJSON('./assistant-vectors.json'),
]);
import { cosine, decodeI8Unit, loadExtractor, embed } from './assistant-embed.js';

const CORPUS = Array.isArray(corpusDoc.chunks) ? corpusDoc.chunks : [];
const ANSWERS = Array.isArray(answersDoc.answers) ? answersDoc.answers : [];

// The suggested questions shown as chips on expand — the highest-value canned ones.
const SUGGESTED_IDS = [
  'can-read-messages',
  'quantum-safe',
  'sealed-sender',
  'reproducible-builds',
  'hide-ip-anonymous',
  'history-new-device',
];

// Below this best lexical score, treat the question as "not covered" rather than guessing.
const MIN_LEXICAL_SCORE = 2.0;
// At most this many source chunks per non-canned answer.
const MAX_CHUNKS = 3;

// ── Semantic tier tuning (only used once the MiniLM embedder has loaded) ──
// A canned answer whose (question+answer) embedding is at least this cosine-similar to the query may
// be surfaced even if lexical matching wasn't confident (recovers paraphrased questions). Calibrated
// against a held-out paraphrase set: at 0.40, correct matches fire with zero wrong answers and
// off-topic questions (max observed cosine ~0.14) never clear the floor.
const SEM_CANNED_FLOOR = 0.4;
// A corpus chunk this cosine-similar to the query may be surfaced when lexical search abstained.
// Kept above the off-topic cosine band so unrelated questions abstain rather than surfacing a
// tangential excerpt.
const SEM_CHUNK_FLOOR = 0.45;
// Fusion weight of the semantic score against the (normalized) lexical score when both are present.
const SEM_WEIGHT = 0.5;

const DISCLAIMER =
  'This assistant is generated from Pollis’s public docs. It can be wrong, and it is not a ' +
  'source of truth — the linked pages are authoritative. Nothing here changes what Pollis does; ' +
  'it only helps you find where the docs say it.';

const STOPWORDS = new Set([
  'a', 'about', 'above', 'after', 'again', 'against', 'all', 'am', 'an', 'and', 'any', 'are', 'as',
  'at', 'be', 'because', 'been', 'before', 'being', 'below', 'between', 'both', 'but', 'by', 'can',
  'could', 'did', 'do', 'does', 'doing', 'done', 'down', 'during', 'each', 'few', 'for', 'from',
  'further', 'get', 'had', 'has', 'have', 'having', 'he', 'her', 'here', 'hers', 'him', 'his', 'how',
  'i', 'if', 'in', 'into', 'is', 'it', 'its', 'just', 'me', 'more', 'most', 'my', 'no', 'nor', 'not',
  'now', 'of', 'off', 'on', 'once', 'only', 'or', 'other', 'our', 'out', 'over', 'own', 'said', 'same',
  'she', 'should', 'so', 'some', 'such', 'than', 'that', 'the', 'their', 'them', 'then', 'there',
  'these', 'they', 'this', 'those', 'through', 'to', 'too', 'under', 'until', 'up', 'us', 'very', 'was',
  'we', 'were', 'what', 'when', 'where', 'which', 'while', 'who', 'whom', 'why', 'will', 'with', 'would',
  'you', 'your', 'yours',
]);

// Lowercase, strip punctuation, split, drop stopwords and one-character tokens.
function tokenize(text) {
  const cleaned = String(text || '').toLowerCase().replace(/[^a-z0-9]+/g, ' ');
  const out = [];
  for (const raw of cleaned.split(' ')) {
    if (raw.length > 1 && !STOPWORDS.has(raw)) {
      out.push(raw);
    }
  }
  return out;
}

// Normalized whitespace-joined form, for substring/alias matching.
function normalize(text) {
  return String(text || '').toLowerCase().replace(/[^a-z0-9]+/g, ' ').replace(/\s+/g, ' ').trim();
}

// ── Precompute lexical index over the corpus (term frequency per chunk + inverse document frequency) ──
const CHUNK_INDEX = CORPUS.map((chunk) => {
  const freq = new Map();
  for (const tok of tokenize(chunk.title + ' ' + chunk.text)) {
    freq.set(tok, (freq.get(tok) || 0) + 1);
  }
  return { chunk, freq };
});

const DOC_FREQ = new Map();
for (const entry of CHUNK_INDEX) {
  for (const tok of entry.freq.keys()) {
    DOC_FREQ.set(tok, (DOC_FREQ.get(tok) || 0) + 1);
  }
}

const N_DOCS = Math.max(CHUNK_INDEX.length, 1);

function idf(token) {
  const df = DOC_FREQ.get(token) || 0;
  if (df === 0) {
    return 0;
  }
  return Math.log(1 + N_DOCS / df);
}

// ── Precompute canned-answer token sets (question + aliases) ──
const ANSWER_INDEX = ANSWERS.map((answer) => {
  const phrases = [answer.question, ...(answer.aliases || [])];
  const tokens = new Set();
  const normalizedPhrases = [];
  for (const phrase of phrases) {
    normalizedPhrases.push(normalize(phrase));
    for (const tok of tokenize(phrase)) {
      tokens.add(tok);
    }
  }
  return { answer, tokens, normalizedPhrases };
});

// ── Semantic tier: lazily-loaded MiniLM re-ranker (extractive, no generative model) ──
// SEM is null until the model loads. When present:
//   { extractor, dim, answerVecs: Map<id,Float32Array>, chunkVecs: Map<id,Float32Array> }
let SEM = null;
let semPromise = null;

// Embed the query with MiniLM (async; runs in-browser via ONNX-Runtime-Web, query never leaves the
// device). Returns a Promise<number[]>.
function embedQuery(query) {
  return embed(SEM.extractor, query);
}

// Kick off the one-time lazy load. Safe to call repeatedly. On any failure, SEM stays null and the
// assistant keeps working as the plain lexical floor.
function ensureEmbedder() {
  if (SEM || semPromise) {
    return semPromise;
  }
  if (typeof fetch !== 'function') {
    return null;
  }
  semPromise = loadExtractor()
    .then((extractor) => installSemantic({ extractor, dim: vectorsDoc.dim }))
    .catch(() => {
      // Stay on the lexical floor; the model is a bonus, never a dependency.
      SEM = null;
    });
  return semPromise;
}

// Decode the precomputed answer/chunk vectors and activate the semantic tier with the given extractor.
// Exported so tests can inject an extractor + dim without a network fetch.
export function installSemantic(model) {
  const dim = model.dim || vectorsDoc.dim || 384;
  const answerVecs = new Map();
  for (const answer of ANSWERS) {
    const b64 = vectorsDoc.answers && vectorsDoc.answers[answer.id];
    if (b64) {
      answerVecs.set(answer.id, decodeI8Unit(b64, dim));
    }
  }
  const chunkVecs = new Map();
  for (const chunk of CORPUS) {
    const b64 = vectorsDoc.chunks && vectorsDoc.chunks[chunk.id];
    if (b64) {
      chunkVecs.set(chunk.id, decodeI8Unit(b64, dim));
    }
  }
  SEM = { extractor: model.extractor, dim, answerVecs, chunkVecs };
  return SEM;
}

// Try to match a curated canned answer. `qVec` is the query embedding when the semantic tier is
// loaded, else null (pure-lexical behaviour — identical to the always-on floor). Returns the answer
// object or null. Semantic scoring only ADDS recall: the lexical-confidence path is unchanged, and an
// exact alias hit (lexComponent 100) always outranks any semantic-only candidate.
function matchCanned(query, qVec) {
  const qNorm = normalize(query);
  const qTokens = tokenize(query);
  if (qTokens.length === 0) {
    return null;
  }
  const qTokenSet = new Set(qTokens);

  let best = null;
  let bestScore = -1;

  for (const entry of ANSWER_INDEX) {
    // A strong signal: a known phrasing appears inside the question, or the question inside a phrasing.
    let aliasHit = false;
    for (const phrase of entry.normalizedPhrases) {
      if (phrase.length >= 4 && (qNorm.includes(phrase) || phrase.includes(qNorm))) {
        aliasHit = true;
        break;
      }
    }

    let overlap = 0;
    for (const tok of qTokenSet) {
      if (entry.tokens.has(tok)) {
        overlap += 1;
      }
    }
    const ratio = overlap / qTokenSet.size;
    const lexConfident = aliasHit || overlap >= 3 || (overlap >= 2 && ratio >= 0.5);

    let sem = 0;
    if (qVec) {
      const v = SEM.answerVecs.get(entry.answer.id);
      if (v) {
        sem = cosine(qVec, v);
      }
    }
    const semConfident = qVec && sem >= SEM_CANNED_FLOOR;

    // A candidate must clear lexical confidence OR strong semantic similarity.
    if (!lexConfident && !semConfident) {
      continue;
    }

    // Fused rank: exact-alias dominates; otherwise semantic (scaled to ~0..50) leads keyword overlap.
    const lexComponent = (aliasHit ? 100 : 0) + overlap + ratio;
    const score = qVec ? lexComponent + SEM_WEIGHT * 100 * sem : lexComponent;
    if (score > bestScore) {
      bestScore = score;
      best = entry.answer;
    }
  }

  return best;
}

// Lexical (or lexical+semantic, when `qVec` is provided) search over the corpus.
// Returns { chunks: [...], closest: chunkOrNull }.
function lexicalSearch(query, qVec) {
  const qTokens = tokenize(query);
  const scored = [];
  let maxLex = MIN_LEXICAL_SCORE;
  for (const entry of CHUNK_INDEX) {
    let lex = 0;
    for (const tok of qTokens) {
      const tf = entry.freq.get(tok);
      if (tf) {
        lex += tf * idf(tok);
      }
    }
    let sem = 0;
    if (qVec) {
      const v = SEM.chunkVecs.get(entry.chunk.id);
      if (v) {
        sem = cosine(qVec, v);
      }
    }
    if (lex > maxLex) {
      maxLex = lex;
    }
    scored.push({ chunk: entry.chunk, lex, sem });
  }

  // Pure-lexical floor (semantic tier not loaded): behave exactly as before.
  if (!qVec) {
    const lexRanked = scored.filter((s) => s.lex > 0).sort((a, b) => b.lex - a.lex);
    const closestLex = lexRanked.length > 0 ? lexRanked[0].chunk : null;
    if (lexRanked.length === 0 || lexRanked[0].lex < MIN_LEXICAL_SCORE) {
      return { chunks: [], closest: closestLex };
    }
    const cut = Math.max(MIN_LEXICAL_SCORE, lexRanked[0].lex * 0.5);
    return {
      chunks: lexRanked.filter((s) => s.lex >= cut).slice(0, MAX_CHUNKS).map((s) => s.chunk),
      closest: closestLex,
    };
  }

  // Hybrid: fuse normalized lexical with semantic; a chunk is eligible if lexical cleared the floor
  // OR it is strongly semantically similar (recovers paraphrased questions lexical missed).
  for (const s of scored) {
    s.fused = SEM_WEIGHT * Math.max(0, s.sem) + (1 - SEM_WEIGHT) * (s.lex / maxLex);
    s.eligible = s.lex >= MIN_LEXICAL_SCORE || s.sem >= SEM_CHUNK_FLOOR;
  }
  const byFused = scored.slice().sort((a, b) => b.fused - a.fused);
  const closest = byFused.length > 0 ? byFused[0].chunk : null;
  const eligible = byFused.filter((s) => s.eligible);
  if (eligible.length === 0) {
    return { chunks: [], closest };
  }
  const cutoff = eligible[0].fused * 0.6;
  const chunks = eligible.filter((s) => s.fused >= cutoff).slice(0, MAX_CHUNKS).map((s) => s.chunk);
  return { chunks, closest };
}

// The one resolution entry point. Embeds the query once (when the semantic tier is loaded) and fuses
// that signal into both the canned matcher and the corpus search. Still fully extractive: every
// surfaced sentence was authored by a human and is shown with its citation.
async function handleQuery(query) {
  const qVec = SEM ? await embedQuery(query) : null;
  // With MiniLM loaded, a paraphrase may route to a doc-exact canned answer, but only above a
  // high-precision cosine floor validated to fire correct answers with zero wrong ones (and to leave
  // off-topic questions below the floor). Without the model, this is the unchanged lexical-only path.
  const canned = matchCanned(query, qVec);
  if (canned) {
    return { kind: 'canned', answer: canned };
  }
  const { chunks, closest } = lexicalSearch(query, qVec);
  if (chunks.length > 0) {
    return { kind: 'chunks', chunks };
  }
  return { kind: 'unknown', closest };
}

// ── Rendering — all text goes through textContent / createElement; links are real <a> elements. ──

// Resolve a citation source to an href. On-site pages are relative filenames; docs are full GitHub URLs.
function hrefForSource(source) {
  return String(source || '');
}

function isExternal(href) {
  return /^https?:/i.test(href);
}

function makeCitationLink(title, source) {
  const link = document.createElement('a');
  link.className = 'assistant-cite';
  link.textContent = title;
  const href = hrefForSource(source);
  link.href = href;
  if (isExternal(href)) {
    link.target = '_blank';
    link.rel = 'noopener noreferrer';
  }
  return link;
}

function makeCitationsBlock(cites) {
  const wrap = document.createElement('div');
  wrap.className = 'assistant-cites';
  const label = document.createElement('span');
  label.className = 'assistant-cites-label';
  label.textContent = 'Source:';
  wrap.appendChild(label);
  for (const cite of cites) {
    wrap.appendChild(makeCitationLink(cite.title, cite.source));
  }
  return wrap;
}

function renderCanned(container, answer) {
  const body = document.createElement('div');
  body.className = 'assistant-answer-body';
  // Answers may contain paragraph breaks authored as blank lines.
  for (const para of String(answer.answer).split(/\n\n+/)) {
    const p = document.createElement('p');
    p.textContent = para.trim();
    body.appendChild(p);
  }
  container.appendChild(body);
  container.appendChild(makeCitationsBlock(answer.cites || []));
}

function renderChunks(container, chunks) {
  const intro = document.createElement('p');
  intro.className = 'assistant-answer-intro';
  intro.textContent = 'Here is what Pollis’s docs say — read the linked source to confirm:';
  container.appendChild(intro);

  for (const chunk of chunks) {
    const block = document.createElement('div');
    block.className = 'assistant-chunk';

    const title = document.createElement('div');
    title.className = 'assistant-chunk-title';
    title.textContent = chunk.title;
    block.appendChild(title);

    const text = document.createElement('p');
    text.className = 'assistant-chunk-text';
    text.textContent = chunk.text;
    block.appendChild(text);

    block.appendChild(makeCitationsBlock([{ title: chunk.source, source: chunk.anchor }]));
    container.appendChild(block);
  }
}

function renderUnknown(container, closest) {
  const p = document.createElement('p');
  p.className = 'assistant-answer-body';
  const fallbackTitle = closest ? closest.source : 'Security';
  const fallbackSource = closest ? closest.anchor : 'security.html';
  p.textContent =
    'I don’t have a doc-backed answer for that. The page most likely to cover it is ';
  const link = makeCitationLink(fallbackTitle, fallbackSource);
  p.appendChild(link);
  const tail = document.createTextNode('. You can also try rephrasing, or pick one of the suggested questions above.');
  p.appendChild(tail);
  container.appendChild(p);
}

// ── Widget construction ──

const REDUCED_MOTION = typeof window !== 'undefined' && window.matchMedia &&
  window.matchMedia('(prefers-reduced-motion: reduce)').matches;

function buildWidget(mount) {
  const panelId = 'assistant-panel';
  const answerId = 'assistant-answer';

  const launcher = document.createElement('button');
  launcher.type = 'button';
  launcher.className = 'assistant-launcher';
  launcher.setAttribute('aria-expanded', 'false');
  launcher.setAttribute('aria-controls', panelId);
  launcher.textContent = 'Ask about how this works';

  const panel = document.createElement('div');
  panel.id = panelId;
  panel.className = 'assistant-panel';
  panel.hidden = true;

  // Persistent honesty disclaimer.
  const disclaimer = document.createElement('p');
  disclaimer.className = 'assistant-disclaimer';
  disclaimer.textContent = DISCLAIMER;
  panel.appendChild(disclaimer);

  // Suggested-question chips.
  const suggestions = document.createElement('div');
  suggestions.className = 'assistant-suggestions';
  suggestions.setAttribute('aria-label', 'Suggested questions');
  const answersById = new Map(ANSWERS.map((a) => [a.id, a]));
  for (const id of SUGGESTED_IDS) {
    const a = answersById.get(id);
    if (!a) {
      continue;
    }
    const chip = document.createElement('button');
    chip.type = 'button';
    chip.className = 'assistant-chip';
    chip.textContent = a.question;
    chip.addEventListener('click', () => {
      input.value = a.question;
      ask(a.question);
    });
    suggestions.appendChild(chip);
  }
  panel.appendChild(suggestions);

  // Question form.
  const form = document.createElement('form');
  form.className = 'assistant-form';

  const label = document.createElement('label');
  label.className = 'assistant-visually-hidden';
  label.setAttribute('for', 'assistant-input');
  label.textContent = 'Ask a question about how Pollis works';
  form.appendChild(label);

  const input = document.createElement('input');
  input.id = 'assistant-input';
  input.className = 'assistant-input';
  input.type = 'text';
  input.autocomplete = 'off';
  input.placeholder = 'e.g. Is it post-quantum? Can Pollis read my messages?';
  form.appendChild(input);

  const submit = document.createElement('button');
  submit.type = 'submit';
  submit.className = 'assistant-submit';
  submit.textContent = 'Ask';
  form.appendChild(submit);

  panel.appendChild(form);

  // Answer area — an aria-live region so answers are announced to assistive tech.
  const answer = document.createElement('div');
  answer.id = answerId;
  answer.className = 'assistant-answer';
  answer.setAttribute('aria-live', 'polite');
  answer.setAttribute('role', 'status');
  panel.appendChild(answer);

  async function ask(query) {
    const trimmed = String(query || '').trim();
    answer.textContent = '';
    if (trimmed.length === 0) {
      const hint = document.createElement('p');
      hint.className = 'assistant-answer-body';
      hint.textContent = 'Type a question, or pick one of the suggestions above.';
      answer.appendChild(hint);
      return;
    }
    // Brief pending state — resolving is async when the MiniLM re-ranker is loaded.
    const pending = document.createElement('p');
    pending.className = 'assistant-answer-body assistant-pending';
    pending.textContent = 'Searching the docs…';
    answer.appendChild(pending);
    submit.disabled = true;

    let result;
    try {
      result = await handleQuery(trimmed);
    } catch (err) {
      result = { kind: 'unknown', closest: null };
    }
    submit.disabled = false;
    answer.textContent = '';
    if (result.kind === 'canned') {
      renderCanned(answer, result.answer);
    } else if (result.kind === 'chunks') {
      renderChunks(answer, result.chunks);
    } else {
      renderUnknown(answer, result.closest);
    }
    if (!REDUCED_MOTION && typeof answer.scrollIntoView === 'function') {
      answer.scrollIntoView({ behavior: 'smooth', block: 'nearest' });
    }
  }

  form.addEventListener('submit', (event) => {
    event.preventDefault();
    ask(input.value);
  });

  function expand() {
    panel.hidden = false;
    launcher.setAttribute('aria-expanded', 'true');
    input.focus();
    // Warm the optional semantic re-ranker in the background. If it never finishes (slow link, fetch
    // blocked, unsupported), queries simply use the instant lexical floor — no worse than before.
    ensureEmbedder();
  }

  function collapse() {
    panel.hidden = true;
    launcher.setAttribute('aria-expanded', 'false');
    launcher.focus();
  }

  launcher.addEventListener('click', () => {
    if (panel.hidden) {
      expand();
    } else {
      collapse();
    }
  });

  // Escape collapses the panel and restores focus to the launcher.
  panel.addEventListener('keydown', (event) => {
    if (event.key === 'Escape') {
      event.preventDefault();
      collapse();
    }
  });

  mount.appendChild(launcher);
  mount.appendChild(panel);
}

function init() {
  const mount = document.getElementById('pollis-assistant');
  if (!mount || mount.dataset.ready === 'true') {
    return;
  }
  mount.dataset.ready = 'true';
  buildWidget(mount);
}

// Export the resolution entry point for testing; the DOM bootstrap only runs in a browser.
export { handleQuery };

if (typeof document !== 'undefined') {
  if (document.readyState === 'loading') {
    document.addEventListener('DOMContentLoaded', init);
  } else {
    init();
  }
}
