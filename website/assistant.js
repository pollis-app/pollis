// Pollis "Ask about how this works" assistant — Phase 0 (retrieval + curated canned answers only).
//
// This is the zero-hallucination-risk floor. It runs ENTIRELY from static JSON already shipped with
// the page: no LLM, no model download, no WebGPU/WASM runtime, no embeddings, and no network calls of
// any kind at query time. The two JSON files are loaded once as JSON modules (no fetch, no CDN, no
// dynamic import) and everything after that is plain in-JS lexical matching.
//
// Answers come from two places, in this order of preference:
//   1. a curated canned-answer map (assistant-answers.json) — the doc-faithful, hand-checked path;
//   2. a simple lexical search over a chunked corpus (assistant-corpus.json), returning source excerpts.
// If neither is confident, the assistant says it does not know and points at the closest page. It never
// synthesizes prose of its own — it only ever surfaces text a human authored and cited.
//
// Phase 1 (a WebGPU generative mode, with a WASM fallback) will add a generative path here later; it is
// intentionally OUT OF SCOPE for this file. The natural seam is `handleQuery()` below.

import corpusDoc from './assistant-corpus.json' with { type: 'json' };
import answersDoc from './assistant-answers.json' with { type: 'json' };

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

// Try to match a curated canned answer. Returns the answer object or null.
function matchCanned(query) {
  const qNorm = normalize(query);
  const qTokens = tokenize(query);
  if (qTokens.length === 0) {
    return null;
  }
  const qTokenSet = new Set(qTokens);

  let best = null;
  let bestScore = 0;

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

    // Confidence: an alias/phrase hit, or a solid keyword overlap.
    const confident = aliasHit || overlap >= 3 || (overlap >= 2 && ratio >= 0.5);
    if (!confident) {
      continue;
    }

    const score = (aliasHit ? 100 : 0) + overlap + ratio;
    if (score > bestScore) {
      bestScore = score;
      best = entry.answer;
    }
  }

  return best;
}

// Lexical search over the corpus. Returns { chunks: [...], closest: chunkOrNull }.
function lexicalSearch(query) {
  const qTokens = tokenize(query);
  const scored = [];
  for (const entry of CHUNK_INDEX) {
    let score = 0;
    for (const tok of qTokens) {
      const tf = entry.freq.get(tok);
      if (tf) {
        score += tf * idf(tok);
      }
    }
    if (score > 0) {
      scored.push({ chunk: entry.chunk, score });
    }
  }
  scored.sort((a, b) => b.score - a.score);

  const closest = scored.length > 0 ? scored[0].chunk : null;
  if (scored.length === 0 || scored[0].score < MIN_LEXICAL_SCORE) {
    return { chunks: [], closest };
  }

  const cutoff = Math.max(MIN_LEXICAL_SCORE, scored[0].score * 0.5);
  const chunks = scored.filter((s) => s.score >= cutoff).slice(0, MAX_CHUNKS).map((s) => s.chunk);
  return { chunks, closest };
}

// The one resolution entry point. Phase 1's generative mode would branch from here (e.g. when a model
// is loaded, hand the retrieved chunks to it as grounding). Phase 0 stays fully extractive.
function handleQuery(query) {
  const canned = matchCanned(query);
  if (canned) {
    return { kind: 'canned', answer: canned };
  }
  const { chunks, closest } = lexicalSearch(query);
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

const REDUCED_MOTION = window.matchMedia && window.matchMedia('(prefers-reduced-motion: reduce)').matches;

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

  function ask(query) {
    const trimmed = String(query || '').trim();
    answer.textContent = '';
    if (trimmed.length === 0) {
      const hint = document.createElement('p');
      hint.className = 'assistant-answer-body';
      hint.textContent = 'Type a question, or pick one of the suggestions above.';
      answer.appendChild(hint);
      return;
    }
    const result = handleQuery(trimmed);
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

if (document.readyState === 'loading') {
  document.addEventListener('DOMContentLoaded', init);
} else {
  init();
}
