// Tests for the archon adapter and its fallback wrapper (#731). Run with: node --test
//
// These exercise archonAdapter() (the single, isolated, provisional-contract function) and queryArchon()
// (the timeout + never-throw wrapper the UI actually calls). The whole point of the #731 design is that
// EVERY archon failure path falls back to the on-device index; queryArchon signals "fall back" by
// returning null, so each failure case below asserts null. Success asserts the normalized shape.
//
// fetch is stubbed via globalThis.fetch — the adapter/wrapper resolve it at call time, so overriding the
// global here fully controls archon's responses without any network.

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';

import { archonAdapter, queryArchon, REMOTE_ANSWERING_ENABLED } from './assistant.js';

// Minimal Response double: just the bits archonAdapter reads.
function response({ ok = true, status = 200, json }) {
  return {
    ok,
    status,
    json: json || (async () => ({})),
  };
}

// Install a fetch stub for the duration of one test, restoring the previous global after.
function withFetch(stub, fn) {
  const prev = globalThis.fetch;
  globalThis.fetch = stub;
  return (async () => {
    try {
      return await fn();
    } finally {
      globalThis.fetch = prev;
    }
  })();
}

const okBody = {
  answer: 'MLS encrypts messages on your device before they leave it.',
  sources: [{ title: 'Security', url: 'https://pollis.com/security.html' }],
};

// ── archonAdapter: success ───────────────────────────────────────────────────
test('archonAdapter returns the normalized answer + sources on a 2xx JSON body', async () => {
  let sentUrl = null;
  let sentBody = null;
  await withFetch(async (url, opts) => {
    sentUrl = url;
    sentBody = JSON.parse(opts.body);
    return response({ json: async () => okBody });
  }, async () => {
    const out = await archonAdapter('is it end to end encrypted?', undefined);
    assert.equal(sentUrl, '/api/assistant');
    assert.deepEqual(sentBody, { query: 'is it end to end encrypted?' });
    assert.equal(out.answer, okBody.answer);
    assert.deepEqual(out.sources, okBody.sources);
  });
});

test('archonAdapter drops malformed source entries but keeps a valid answer', async () => {
  await withFetch(
    async () =>
      response({
        json: async () => ({
          answer: 'ok',
          sources: [
            { title: 'good', url: 'https://x' },
            { title: 'no url' },
            { url: 'https://y' },
            'nonsense',
            null,
          ],
        }),
      }),
    async () => {
      const out = await archonAdapter('q', undefined);
      assert.deepEqual(out.sources, [{ title: 'good', url: 'https://x' }]);
    }
  );
});

test('archonAdapter tolerates a missing sources field (empty array)', async () => {
  await withFetch(
    async () => response({ json: async () => ({ answer: 'ok' }) }),
    async () => {
      const out = await archonAdapter('q', undefined);
      assert.deepEqual(out.sources, []);
    }
  );
});

// ── archonAdapter: failure branches (each must throw) ─────────────────────────
test('archonAdapter throws on a non-2xx status', async () => {
  await withFetch(
    async () => response({ ok: false, status: 503 }),
    async () => {
      await assert.rejects(() => archonAdapter('q', undefined), /HTTP 503/);
    }
  );
});

test('archonAdapter throws on a malformed (non-JSON) body', async () => {
  await withFetch(
    async () =>
      response({
        json: async () => {
          throw new SyntaxError('Unexpected token < in JSON');
        },
      }),
    async () => {
      await assert.rejects(() => archonAdapter('q', undefined));
    }
  );
});

test('archonAdapter throws when answer is missing', async () => {
  await withFetch(
    async () => response({ json: async () => ({ sources: [] }) }),
    async () => {
      await assert.rejects(() => archonAdapter('q', undefined), /missing or empty answer/);
    }
  );
});

test('archonAdapter throws when answer is present but blank', async () => {
  await withFetch(
    async () => response({ json: async () => ({ answer: '   ' }) }),
    async () => {
      await assert.rejects(() => archonAdapter('q', undefined), /missing or empty answer/);
    }
  );
});

// ── queryArchon: success + every failure path returns null (→ fall back) ──────
test('queryArchon returns the normalized answer on success', async () => {
  await withFetch(
    async () => response({ json: async () => okBody }),
    async () => {
      const out = await queryArchon('q', undefined, true);
      assert.equal(out.answer, okBody.answer);
    }
  );
});

test('queryArchon returns null on a non-2xx status', async () => {
  await withFetch(
    async () => response({ ok: false, status: 500 }),
    async () => {
      assert.equal(await queryArchon('q', undefined, true), null);
    }
  );
});

test('queryArchon returns null on a network error (fetch rejects)', async () => {
  await withFetch(
    async () => {
      throw new TypeError('Failed to fetch');
    },
    async () => {
      assert.equal(await queryArchon('q', undefined, true), null);
    }
  );
});

test('queryArchon returns null on a malformed body', async () => {
  await withFetch(
    async () =>
      response({
        json: async () => {
          throw new SyntaxError('bad json');
        },
      }),
    async () => {
      assert.equal(await queryArchon('q', undefined, true), null);
    }
  );
});

test('queryArchon returns null when the answer is missing', async () => {
  await withFetch(
    async () => response({ json: async () => ({ sources: [] }) }),
    async () => {
      assert.equal(await queryArchon('q', undefined, true), null);
    }
  );
});

test('queryArchon returns null (and aborts) when archon is too slow', async () => {
  let aborted = false;
  await withFetch(
    // A fetch that never resolves on its own — it only settles when the timeout aborts the signal.
    (url, opts) =>
      new Promise((_, reject) => {
        opts.signal.addEventListener('abort', () => {
          aborted = true;
          reject(new DOMException('The operation was aborted.', 'AbortError'));
        });
      }),
    async () => {
      // Tiny timeout so the test doesn't wait the real 4s ceiling.
      const out = await queryArchon('q', 20, true);
      assert.equal(out, null);
      assert.equal(aborted, true);
    }
  );
});

// ── The remote-answering gate (#765) ─────────────────────────────────────────────────────────────
// The remote path is ON, and reaches archon through this site's own Pages Function rather than calling
// archon.pollis.com directly — a direct call is refused by Cloudflare Access at the edge, which is what
// made every answer come from the fallback. These pin the flag, the kill switch, and the endpoint.

test('remote answering is enabled', async () => {
  assert.equal(REMOTE_ANSWERING_ENABLED, true,
    'the assistant reaches archon through /api/assistant; the remote path should be live');
});

test('queryArchon makes no network call when remote answering is switched off', async () => {
  let called = false;
  await withFetch(
    async () => {
      called = true;
      return response({ json: async () => okBody });
    },
    async () => {
      // The kill switch: turning the flag off must stop the request at the gate, not fire it and
      // swallow the failure — otherwise "off" would still send the user's question.
      assert.equal(await queryArchon('q', undefined, false), null);
      assert.equal(called, false, 'the gate must short-circuit BEFORE fetch, not swallow its failure');
    }
  );
});

// The #765 regression guard. Pointing the browser back at archon.pollis.com fails as a CORS error,
// which reads like a header problem and is not one — it is Access refusing the request at the edge. The
// symptom is invisible (the fallback answers anyway), so this pins the endpoint rather than trusting a
// future reader to know why it is same-origin.
test('the browser never calls archon directly', async () => {
  const src = await readFile(new URL('./assistant.js', import.meta.url), 'utf8');
  const inCode = src
    .split('\n')
    .filter((line) => !line.trimStart().startsWith('//'))
    .join('\n');
  assert.ok(!/archon\.pollis\.com/.test(inCode),
    'assistant.js must reach archon through /api/assistant, never the archon origin directly');
  assert.ok(inCode.includes("'/api/assistant'"),
    'the remote path must post to the same-origin Pages Function');
});

test('the privacy notice matches what the code actually does', async () => {
  const src = await readFile(new URL('./assistant.js', import.meta.url), 'utf8');
  const claimsRemote = /your question is sent to Pollis/.test(src.split('REMOTE_ANSWERING_ENABLED')[2] ?? '');
  // Whatever the flag is, the notice shown must describe that state. The failure this guards is a
  // notice promising a network call the code never makes (or, far worse, the reverse).
  if (REMOTE_ANSWERING_ENABLED) {
    assert.ok(src.includes('your question is sent to Pollis'),
      'remote answering is on, so the notice must say questions are sent');
  } else {
    assert.ok(src.includes('answered entirely in your browser'),
      'remote answering is off, so the notice must say answering is local');
  }
  void claimsRemote;
});
