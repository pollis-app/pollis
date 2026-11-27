/* Message formatting (#2142): design/messageFormat.ts against the shared case table, which
   pollis-core/src/message_format.rs also runs. mobile/tests/message-format.test.ts runs it too. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

import {
  parseFormatting,
  plainText,
  scanText,
  stripFormatting,
  type FormatNode,
} from "../../design/messageFormat.ts";

type Compact = string | { [key: string]: unknown };
interface Case {
  name: string;
  input: string;
  tokens: Compact[];
  plain: string;
  scan?: string;
}

const table = JSON.parse(
  readFileSync(new URL("../../design/messageFormat.cases.json", import.meta.url), "utf8"),
) as { cases: Case[] };

const SHORT = { bold: "b", italic: "i", underline: "u", strike: "s" } as const;

/** The case table's compact form of a tree. */
function compact(nodes: FormatNode[]): Compact[] {
  return nodes.map((n) => {
    if (n.type === "text") {
      return n.text;
    }
    if (n.type === "code") {
      return { code: n.text, lang: n.lang };
    }
    return { [SHORT[n.type]]: compact(n.children) };
  });
}

for (const c of table.cases) {
  test(c.name, () => {
    const nodes = parseFormatting(c.input);
    assert.deepEqual(compact(nodes), c.tokens);
    assert.equal(plainText(nodes), c.plain);
    assert.equal(stripFormatting(c.input), c.plain);
    if (c.scan !== undefined) {
      assert.equal(scanText(c.input), c.scan);
    }
  });
}

test("the table is not trivially small", () => {
  assert.ok(table.cases.length >= 50);
});

test("a pathological run of openers stays fast", () => {
  const text = "*a _b ~~c **d __e ".repeat(4000);
  const started = Date.now();
  const nodes = parseFormatting(text);
  assert.ok(Date.now() - started < 1500, "parse took too long");
  assert.equal(plainText(nodes), text);
});

test("deep nesting stops at the depth limit and keeps every character", () => {
  const text = `${"*_".repeat(40)}x${"_*".repeat(40)}`;
  const nodes = parseFormatting(text);
  assert.ok(plainText(nodes).includes("x"));
});
