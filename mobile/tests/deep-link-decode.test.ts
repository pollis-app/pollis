import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";

// patches/decode-uri-component@0.2.2.patch backports 0.5.1's linear decoder (GHSA-vcc3-ghjq-m6fr, GHSA-qr28-fpjm-wq7f).
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

// GHSA-qr28-fpjm-wq7f: many distinct percent runs plus one malformed byte made
// 0.5.0's replace-map loop run one full-string regex per run (~20s here).
test("many distinct malformed percent runs decode in linear time", () => {
  let hostile = "";
  for (let i = 0; i < 32000; i++) {
    const high = Math.floor(i / 256).toString(16).padStart(2, "0");
    const low = (i % 256).toString(16).padStart(2, "0");
    hostile += `%${high}%${low}x`;
  }
  hostile += "%zz";
  const start = Date.now();
  decode(hostile);
  assert.ok(Date.now() - start < 1000, `took ${Date.now() - start}ms`);
});

test("valid and partially malformed input decodes as before", () => {
  assert.equal(decode("%C2"), "�");
  assert.equal(decode("x%FF%FEy%zz"), "x��y%zz");
  assert.equal(decode("caf%C3%A9"), "café");
  assert.equal(decode("a+b"), "a b");
  assert.equal(decode("%E2%82%AC%ff%41"), "€%ffA");
  assert.equal(decode("%"), "%");
  assert.equal(decode("%FE%FF"), "\uFFFD\uFFFD");
  assert.deepEqual({ ...queryString.parse("groupId=g_1&name=caf%C3%A9") }, { groupId: "g_1", name: "café" });
});
