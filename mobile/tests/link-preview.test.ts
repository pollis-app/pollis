/* Sender-generated link previews (#1987): the same cases as frontend/tests/link-preview.test.ts, and that mobile's copy is desktop's file exactly. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import {
  LinkPreviewComposer,
  PREVIEW_DEBOUNCE_MS,
  matchPreviewLink,
  previewCandidate,
  type LinkPreviewDraft,
  type LinkPreviewState,
} from "../lib/linkPreview.ts";

const here = dirname(fileURLToPath(import.meta.url));

test("mobile/lib/linkPreview.ts is desktop's linkPreview.ts, byte for byte", () => {
  const mobile = readFileSync(join(here, "../lib/linkPreview.ts"), "utf8");
  const desktop = readFileSync(join(here, "../../frontend/src/utils/linkPreview.ts"), "utf8");
  assert.equal(mobile, desktop, "edit frontend/src/utils/linkPreview.ts and copy it to mobile/lib/linkPreview.ts");
});

test("only the first https link the finder accepts is a candidate", () => {
  assert.equal(previewCandidate("no links here"), null);
  assert.equal(previewCandidate("see https://example.com/a and https://example.org/b"), "https://example.com/a");
  // http is never fetched; the first https link after it is.
  assert.equal(previewCandidate("http://plain.example.com then https://secure.example.com"), "https://secure.example.com");
  assert.equal(previewCandidate("http://plain.example.com only"), null);
  // `www.` opens over https, normalized the way the linkifier opens it.
  assert.equal(previewCandidate("www.example.com/x"), "https://www.example.com/x");
  // Credentials and invisible controls are not links at all.
  assert.equal(previewCandidate("https://user:pw@example.com"), null);
  assert.equal(previewCandidate("https://exa‮mple.com"), null);
  // Normalized: uppercase scheme, IDN host.
  assert.equal(previewCandidate("HTTPS://Example.com/Path"), "https://example.com/Path");
});

test("a received preview is drawn only for an exact body link, with the body's host", () => {
  const body = "look at https://example.com/a, nice";
  assert.deepEqual(matchPreviewLink(body, { url: "https://example.com/a" }), {
    href: "https://example.com/a",
    host: "example.com",
  });
  // Any other URL, however close, draws nothing.
  assert.equal(matchPreviewLink(body, { url: "https://example.com/a/" }), null);
  assert.equal(matchPreviewLink(body, { url: "https://evil.example/a" }), null);
  assert.equal(matchPreviewLink(body, { url: "http://example.com/a" }), null);
  assert.equal(matchPreviewLink("no link", { url: "https://example.com/a" }), null);
  assert.equal(matchPreviewLink(body, null), null);
  assert.equal(matchPreviewLink(undefined, { url: "https://example.com/a" }), null);
  // The same normalization on both sides: `www.` and an IDN host.
  assert.deepEqual(matchPreviewLink("www.example.com/x", { url: "https://www.example.com/x" }), {
    href: "https://www.example.com/x",
    host: "www.example.com",
  });
  const idn = matchPreviewLink("https://bücher.example/x", { url: "https://xn--bcher-kva.example/x" });
  assert.equal(idn?.host, "xn--bcher-kva.example");
  // A second link may carry the preview if it is the one the sender's device matched.
  assert.equal(
    matchPreviewLink("http://a.example https://b.example/p", { url: "https://b.example/p" })?.host,
    "b.example",
  );
});

/** A composer with a manual clock and scripted fetches. */
function harness() {
  const states: LinkPreviewState[] = [];
  const fetched: string[] = [];
  const discarded: string[] = [];
  const timers: { run: () => void; ms: number; live: boolean }[] = [];
  const answers = new Map<string, (d: LinkPreviewDraft | null) => void>();
  const composer = new LinkPreviewComposer({
    fetch: (url) => {
      fetched.push(url);
      return new Promise((resolve) => answers.set(url, resolve));
    },
    discard: (id) => discarded.push(id),
    onChange: (s) => states.push(s),
    schedule: (run, ms) => {
      const t = { run, ms, live: true };
      timers.push(t);
      return t;
    },
    cancel: (handle) => {
      (handle as { live: boolean }).live = false;
    },
  });
  const tick = () => {
    for (const t of timers.splice(0)) {
      if (t.live) {
        t.run();
      }
    }
  };
  const answer = async (url: string, draft: LinkPreviewDraft | null) => {
    answers.get(url)?.(draft);
    await new Promise((r) => setImmediate(r));
  };
  return { composer, states, fetched, discarded, timers, tick, answer };
}

const draft = (url: string, stagedId: string | null = "s1"): LinkPreviewDraft => ({
  url,
  title: "Title",
  description: null,
  image: stagedId ? { stagedId, width: 10, height: 10, dataUrl: "data:image/jpeg;base64,AA==" } : null,
});

test("a paste fetches at once and shows loading on the same call", () => {
  const h = harness();
  h.composer.update("https://example.com/a", true);
  assert.deepEqual(h.fetched, ["https://example.com/a"]);
  assert.deepEqual(h.composer.current, { status: "loading", url: "https://example.com/a" });
});

test("typing waits for the debounce, and the same URL is never fetched twice", async () => {
  const h = harness();
  let text = "";
  for (const ch of "https://example.com") {
    text += ch;
    h.composer.update(text, true);
  }
  assert.deepEqual(h.fetched, []);
  assert.equal(h.timers.filter((t) => t.live).length, 1);
  assert.equal(h.timers.find((t) => t.live)?.ms, PREVIEW_DEBOUNCE_MS);
  h.tick();
  assert.deepEqual(h.fetched, ["https://example.com"]);
  await h.answer("https://example.com", draft("https://example.com"));
  assert.equal(h.composer.current.status, "ready");
  h.composer.update(`${text} more words`, true);
  h.tick();
  assert.deepEqual(h.fetched, ["https://example.com"]);
});

test("a fetch that finds nothing is not retried for the same link", async () => {
  const h = harness();
  h.composer.update("https://example.com/x", true);
  await h.answer("https://example.com/x", null);
  assert.equal(h.composer.current.status, "none");
  h.composer.update("https://example.com/x hi", true);
  assert.deepEqual(h.fetched, ["https://example.com/x"]);
});

test("a changed first link discards the old draft and fetches the new one", async () => {
  const h = harness();
  h.composer.update("https://a.example/1", true);
  await h.answer("https://a.example/1", draft("https://a.example/1", "old"));
  // Same length as before, so typed, not pasted: it waits for the debounce.
  h.composer.update("https://b.example/2", true);
  assert.deepEqual(h.discarded, ["old"]);
  h.tick();
  assert.deepEqual(h.fetched, ["https://a.example/1", "https://b.example/2"]);
  // A late answer for a link no longer wanted is released, never shown.
  h.composer.update("nothing now", true);
  await h.answer("https://b.example/2", draft("https://b.example/2", "late"));
  assert.deepEqual(h.discarded, ["old", "late"]);
  assert.equal(h.composer.current.status, "none");
});

test("× suppresses that URL for the rest of the draft; a reset forgets it", async () => {
  const h = harness();
  h.composer.update("https://example.com", true);
  await h.answer("https://example.com", draft("https://example.com", "x"));
  h.composer.remove();
  assert.deepEqual(h.discarded, ["x"]);
  assert.equal(h.composer.current.status, "none");
  h.composer.update("https://example.com and more", true);
  h.tick();
  assert.deepEqual(h.fetched, ["https://example.com"]);
  h.composer.reset();
  h.composer.update("https://example.com", true);
  assert.deepEqual(h.fetched, ["https://example.com", "https://example.com"]);
});

test("send takes a finished preview only while its link is in the text, never waits", async () => {
  const h = harness();
  h.composer.update("https://example.com/p", true);
  // Still loading: sent without it, and the late answer is released.
  assert.equal(h.composer.takeForSend("https://example.com/p"), null);
  await h.answer("https://example.com/p", draft("https://example.com/p", "late"));
  assert.deepEqual(h.discarded, ["late"]);

  h.composer.update("https://example.com/p", true);
  await h.answer("https://example.com/p", draft("https://example.com/p", "ok"));
  const taken = h.composer.takeForSend("text https://example.com/p");
  assert.equal(taken?.url, "https://example.com/p");
  // Consumed by the core on send: not discarded here, and the card is gone at once.
  assert.deepEqual(h.discarded, ["late"]);
  assert.equal(h.composer.current.status, "none");
  assert.equal(h.states[h.states.length - 1].status, "none");

  h.composer.update("https://example.com/q", true);
  await h.answer("https://example.com/q", draft("https://example.com/q", "gone"));
  assert.equal(h.composer.takeForSend("the link was deleted"), null);
  assert.deepEqual(h.discarded, ["late", "gone"]);
});

test("the draft's url is pinned to the link as written in the body", async () => {
  const h = harness();
  h.composer.update("https://example.com/p", true);
  await h.answer("https://example.com/p", draft("https://example.com/p/redirected", null));
  const state = h.composer.current;
  assert.equal(state.status === "ready" ? state.draft.url : null, "https://example.com/p");
});

test("disabled (setting off, Terminal look) never fetches and drops a shown card", async () => {
  const h = harness();
  h.composer.update("https://example.com", false);
  h.tick();
  assert.deepEqual(h.fetched, []);
  h.composer.update("https://example.com", true);
  h.tick();
  await h.answer("https://example.com", draft("https://example.com", "s"));
  h.composer.update("https://example.com", false);
  assert.deepEqual(h.discarded, ["s"]);
  assert.equal(h.composer.current.status, "none");
});
