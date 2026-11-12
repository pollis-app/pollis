#!/usr/bin/env node
/**
 * test-website-copy.mjs — website/copy.js, the site's one copy button (#1731).
 *
 * The website has no build step and no test runner, so this loads copy.js into
 * a VM context with the few DOM pieces it touches (a click listener, closest(),
 * attributes, textContent, getElementById, the clipboard) and drives it.
 *
 * Run: node --test scripts/test-website-copy.mjs
 */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";

const source = readFileSync(join(dirname(fileURLToPath(import.meta.url)), "../website/copy.js"), "utf8");

class El {
  constructor(tag, attrs = {}, text = "") {
    this.tagName = tag.toUpperCase();
    this.attrs = { ...attrs };
    this.textContent = text;
    this.parent = null;
  }
  hasAttribute(n) {
    return n in this.attrs;
  }
  getAttribute(n) {
    return n in this.attrs ? this.attrs[n] : null;
  }
  setAttribute(n, v) {
    this.attrs[n] = String(v);
  }
  removeAttribute(n) {
    delete this.attrs[n];
  }
  // Only the selector copy.js uses: "button[data-copy], button[data-copy-from]".
  closest() {
    for (let el = this; el; el = el.parent) {
      if (el.tagName === "BUTTON" && (el.hasAttribute("data-copy") || el.hasAttribute("data-copy-from"))) {
        return el;
      }
    }
    return null;
  }
}

function page({ clipboard = true, deny = false } = {}) {
  const byId = new Map();
  const timers = [];
  let listener = null;
  const written = [];
  const ctx = {
    document: {
      addEventListener(type, fn) {
        assert.equal(type, "click");
        listener = fn;
      },
      getElementById: (id) => byId.get(id) ?? null,
    },
    navigator: clipboard
      ? {
          clipboard: {
            writeText(v) {
              written.push(v);
              return deny ? Promise.reject(new Error("denied")) : Promise.resolve();
            },
          },
        }
      : {},
    setTimeout(fn, ms) {
      timers.push({ fn, ms, live: true });
      return timers.length;
    },
    clearTimeout(id) {
      timers[id - 1].live = false;
    },
  };
  vm.runInNewContext(source, ctx);
  return {
    byId,
    written,
    timers,
    async click(target) {
      listener({ target });
      await new Promise((r) => setImmediate(r));
    },
    fire() {
      for (const t of timers.filter((x) => x.live)) {
        t.live = false;
        t.fn();
      }
    },
  };
}

test("data-copy copies the attribute value and marks the button for 1.5 s", async () => {
  const p = page();
  const btn = new El("button", { "data-copy": "abc123…full" }, "abc…");
  await p.click(btn);
  assert.deepEqual(p.written, ["abc123…full"]);
  assert.ok(btn.hasAttribute("data-copied"));
  assert.equal(p.timers[0].ms, 1500);
  p.fire();
  assert.ok(!btn.hasAttribute("data-copied"));
  assert.equal(btn.textContent, "abc…");
});

test("data-copy-from copies the named element's text (install command)", async () => {
  const p = page();
  p.byId.set("install-cmd", new El("code", {}, "curl -fsSL https://example/install.sh | bash"));
  const btn = new El("button", { "data-copy-from": "install-cmd" });
  const icon = new El("svg");
  icon.parent = btn;
  await p.click(icon);
  assert.deepEqual(p.written, ["curl -fsSL https://example/install.sh | bash"]);
  assert.ok(btn.hasAttribute("data-copied"));
});

test("data-copied-label swaps the text and restores it, also after a double click", async () => {
  const p = page();
  const btn = new El("button", { "data-copy": "ff00", "data-copied-label": "Copied" }, "Copy");
  await p.click(btn);
  assert.equal(btn.textContent, "Copied");
  await p.click(btn);
  assert.equal(btn.textContent, "Copied");
  p.fire();
  assert.equal(btn.textContent, "Copy");
  assert.ok(!btn.hasAttribute("data-copied"));
});

test("clicks elsewhere, a missing clipboard and a denied clipboard change nothing", async () => {
  const p = page();
  await p.click(new El("button", {}, "Other"));
  assert.deepEqual(p.written, []);

  const none = page({ clipboard: false });
  const a = new El("button", { "data-copy": "x" });
  await none.click(a);
  assert.ok(!a.hasAttribute("data-copied"));

  const denied = page({ deny: true });
  const b = new El("button", { "data-copy": "x", "data-copied-label": "Copied" }, "Copy");
  await denied.click(b);
  assert.deepEqual(denied.written, ["x"]);
  assert.ok(!b.hasAttribute("data-copied"));
  assert.equal(b.textContent, "Copy");
});
