import { test } from "node:test";
import assert from "node:assert/strict";
import { createRequire } from "node:module";
import decodeUpstream from "decode-uri-component-upstream";

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

// The backport must stay byte-for-byte 0.5.1 (devDependency alias), so the
// advisories' fixed range covers it. 0.2.2's wrapper also maps `+` to a space.
test("matches decode-uri-component 0.5.1 on random percent-heavy input", () => {
  const pieces = [
    "%", "%%", "%2", "%G1", "%zz", "x", "+", " ", "é", "😀", "%41", "%25", "%2B",
    "%C2", "%C3%A9", "%E2%82%AC", "%F0%9F%98%80", "%E2%82", "%F0%9F", "%80", "%BF",
    "%C0", "%C1", "%ED%A0%80", "%F4%90%80%80", "%F5", "%FE", "%FF", "%FE%FF", "%ff%fe",
    "%e2%82%ac", "%c2%a0",
  ];
  let seed = 0x1652;
  const next = () => {
    seed = (Math.imul(seed, 1103515245) + 12345) >>> 0;
    return seed;
  };
  for (let i = 0; i < 20_000; i++) {
    let input = "";
    const length = next() % 24;
    for (let j = 0; j < length; j++) {
      input += pieces[next() % pieces.length];
    }
    assert.equal(decode(input), decodeUpstream(input.replace(/\+/g, " ")), JSON.stringify(input));
  }
});
