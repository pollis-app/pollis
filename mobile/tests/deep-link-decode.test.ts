import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

// patches/decode-uri-component@0.2.2.patch backports a linear decoder (GHSA-vcc3-ghjq-m6fr).
const require = createRequire(import.meta.url);
const queryString = require("query-string") as { parse: (q: string) => Record<string, unknown> };
const decode = require(
  require.resolve("decode-uri-component", { paths: [require.resolve("query-string")] }),
) as (s: string) => string;

test("a 300KB malformed percent run decodes in linear time", () => {
  const hostile = "%ff".repeat(100_000);
  const start = Date.now();
  decode(hostile);
  queryString.parse(`a=${hostile}`);
  assert.ok(Date.now() - start < 1000, `took ${Date.now() - start}ms`);
});

test("valid and partially malformed input decodes as before", () => {
  assert.equal(decode("caf%C3%A9"), "café");
  assert.equal(decode("a+b"), "a b");
  assert.equal(decode("%E2%82%AC%ff%41"), "€%ffA");
  assert.equal(decode("%"), "%");
  assert.equal(decode("%FE%FF"), "\uFFFD\uFFFD");
  assert.deepEqual({ ...queryString.parse("groupId=g_1&name=caf%C3%A9") }, { groupId: "g_1", name: "café" });
});
