/* The enrollment SAS input rule (#1096) and its desktop twin; Rust's SAS_ALPHABET / SAS_LEN are the source of truth. */

import test from "node:test";
import assert from "node:assert/strict";

import {
  SAS_ALPHABET,
  SAS_LENGTH,
  normalizeSasInput,
} from "../lib/enrollmentSas.ts";

// Quoted verbatim from `device_enrollment.rs`:
//   const SAS_ALPHABET: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
//   const SAS_LEN: usize = 8;
const RUST_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ";
const RUST_LEN = 8;

test("the alphabet and length match the Rust constants", () => {
  assert.equal(SAS_ALPHABET, RUST_ALPHABET);
  assert.equal(SAS_LENGTH, RUST_LEN);
  assert.equal(SAS_ALPHABET.length, 32, "a base32 alphabet has 32 symbols");
  assert.equal(
    new Set(SAS_ALPHABET).size,
    32,
    "a repeated symbol would make two codes compare equal",
  );
});

test("the excluded glyphs really are excluded", () => {
  // Crockford's exclusions, the reason a code can be read aloud safely.
  for (const ch of ["I", "L", "O", "U"]) {
    assert.ok(
      !SAS_ALPHABET.includes(ch),
      `${ch} is ambiguous and must not appear in a code`,
    );
  }
});

test("normalization upper-cases and keeps the order", () => {
  assert.equal(normalizeSasInput("abcd2345"), "ABCD2345");
  assert.equal(normalizeSasInput("ABCD2345"), "ABCD2345");
});

test("separators a human adds while reading aloud are dropped", () => {
  assert.equal(normalizeSasInput("ABCD 2345"), "ABCD2345");
  assert.equal(normalizeSasInput("ABCD-2345"), "ABCD2345");
  assert.equal(normalizeSasInput(" abcd\t2345\n"), "ABCD2345");
});

test("an excluded glyph is DROPPED, never mapped to a lookalike", () => {
  // O→0 / I→1 would let a mis-read code compare equal: never.
  assert.equal(normalizeSasInput("O0"), "0");
  assert.equal(normalizeSasInput("I1"), "1");
  assert.equal(normalizeSasInput("L1"), "1");
  assert.equal(normalizeSasInput("UV"), "V");
});

test("input is truncated to the code length", () => {
  assert.equal(normalizeSasInput("ABCD2345EXTRA"), "ABCD2345");
  assert.equal(normalizeSasInput("ABCD2345").length, SAS_LENGTH);
});

test("a partial code stays partial, so the approve button stays gated", () => {
  assert.equal(normalizeSasInput("ABC"), "ABC");
  assert.ok(normalizeSasInput("ABC").length < SAS_LENGTH);
  // Junk-only input yields nothing rather than a short "valid-looking" code.
  assert.equal(normalizeSasInput("!!! ??? ---"), "");
  assert.equal(normalizeSasInput("oilu"), "");
});
