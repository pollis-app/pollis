/*
 * `:shortcode:` parsing, resolution and ranking for the composer.
 *
 * This is the half of the feature a browser spec cannot pin cheaply and the
 * half where the bugs are: what counts as a trigger, what a shortcode may
 * contain, and which of two emoji claiming the same name wins. The rules it
 * has to hold:
 *
 *   - a ':' opens a query only at the start of the string or after whitespace,
 *     which is what makes `http://example.com` and `10:30` inert;
 *   - the body alphabet is `[A-Za-z0-9_+-]` — wider than a CUSTOM shortcode,
 *     which stays `[a-z0-9_]{2,32}` and must not be widened by this;
 *   - custom emoji beat standard ones, in the list and in substitution.
 *
 * The last test walks the REAL generated table, so a regeneration that lost
 * the shortcode column fails here rather than silently in the UI.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  SHORTCODE_MIN_QUERY,
  applyShortcode,
  completedShortcodeAt,
  rankShortcodeEntries,
  resolveShortcode,
  shortcodeQueryAt,
  type ShortcodeEntry,
} from "../src/components/Emoji/emojiShortcodeQuery.ts";
import { SHORTCODE_RE } from "../src/components/Emoji/emojiShortcode.ts";
import { STANDARD_EMOJI } from "../src/components/Emoji/emojiData.ts";

function standard(shortcode: string, char: string): ShortcodeEntry {
  return {
    shortcode,
    insertText: char,
    custom: false,
    char,
    contentHash: null,
    label: shortcode,
  };
}

function custom(shortcode: string): ShortcodeEntry {
  const hash = "a".repeat(64);
  return {
    shortcode,
    insertText: `<:${shortcode}:${hash}>`,
    custom: true,
    char: null,
    contentHash: hash,
    label: "design",
  };
}

// ── shortcodeQueryAt ────────────────────────────────────────────────────────

test("a colon at a word start opens a query", () => {
  assert.deepEqual(shortcodeQueryAt(":jo", 3), { start: 0, end: 3, query: "jo" });
  assert.deepEqual(shortcodeQueryAt("nice :ta", 8), { start: 5, end: 8, query: "ta" });
  // Right after the colon the query is empty but the track IS open — that is
  // what makes the composer start loading the table before the second letter.
  assert.deepEqual(shortcodeQueryAt("hey :", 5), { start: 4, end: 5, query: "" });
});

test("a colon mid-word never opens a query", () => {
  // The two cases the trigger rule exists for.
  assert.equal(shortcodeQueryAt("http://example.com", 7), null);
  assert.equal(shortcodeQueryAt("http://example.com", 8), null);
  assert.equal(shortcodeQueryAt("meet at 10:30", 13), null);
  assert.equal(shortcodeQueryAt("meet at 10:3", 12), null);
  // …and the near miss inside a wire token.
  assert.equal(shortcodeQueryAt("<:partyparrot", 13), null);
});

test("a query ends at whitespace and at a second colon", () => {
  assert.equal(shortcodeQueryAt(":joy ", 5), null);
  assert.equal(shortcodeQueryAt(":joy:", 5), null);
  assert.equal(shortcodeQueryAt("plain text", 10), null);
  assert.equal(shortcodeQueryAt("", 0), null);
});

test("the body alphabet carries + and -, and the query is lowercased", () => {
  assert.equal(shortcodeQueryAt(":+1", 3)?.query, "+1");
  assert.equal(shortcodeQueryAt(":e-mail", 7)?.query, "e-mail");
  assert.equal(shortcodeQueryAt(":JOY", 4)?.query, "joy");
  // A smiley is not a query: ')' is not a body character, so the run is empty.
  assert.equal(shortcodeQueryAt(":)", 2), null);
});

test("widening the composer alphabet did not widen a custom shortcode", () => {
  // The composer will happily look ":+1" up; the DS still refuses to store it.
  assert.equal(SHORTCODE_RE.test("+1"), false);
  assert.equal(SHORTCODE_RE.test("e-mail"), false);
  assert.equal(SHORTCODE_RE.test("party_parrot"), true);
});

// ── completedShortcodeAt ────────────────────────────────────────────────────

test("a closed :name: is found at the caret", () => {
  assert.deepEqual(completedShortcodeAt(":joy:", 5), { start: 0, end: 5, name: "joy" });
  assert.deepEqual(completedShortcodeAt("ship it :tada:", 14), {
    start: 8,
    end: 14,
    name: "tada",
  });
  // One-character aliases are real gemoji names — the two-character floor is a
  // suggestion-list rule, not a validity rule.
  assert.deepEqual(completedShortcodeAt(":v:", 3), { start: 0, end: 3, name: "v" });
});

test("nothing else closes a shortcode", () => {
  assert.equal(completedShortcodeAt("::", 2), null);
  assert.equal(completedShortcodeAt(":joy", 4), null);
  assert.equal(completedShortcodeAt("10:30:", 6), null);
  assert.equal(completedShortcodeAt("http://x:", 9), null);
  assert.equal(completedShortcodeAt(":joy: ", 6), null);
  assert.equal(completedShortcodeAt("", 0), null);
});

// ── resolution: custom beats standard ───────────────────────────────────────

test("a custom emoji wins its name outright", () => {
  const entries = [standard("tada", "🎉"), custom("tada")];
  const hit = resolveShortcode(entries, "tada");
  assert.equal(hit?.custom, true);
  assert.match(hit!.insertText, /^<:tada:[0-9a-f]{64}>$/);

  // …in either input order: the rule is the flag, not the position.
  const flipped = resolveShortcode([custom("tada"), standard("tada", "🎉")], "tada");
  assert.equal(flipped?.custom, true);
});

test("an unclaimed name still resolves to the standard emoji", () => {
  const entries = [standard("joy", "😂"), custom("shipit")];
  assert.equal(resolveShortcode(entries, "joy")?.insertText, "😂");
  assert.equal(resolveShortcode(entries, "nope"), undefined);
});

// ── ranking ─────────────────────────────────────────────────────────────────

test("nothing is suggested below the minimum query length", () => {
  const entries = [standard("joy", "😂")];
  assert.equal(SHORTCODE_MIN_QUERY, 2);
  assert.deepEqual(rankShortcodeEntries(entries, "j"), []);
  assert.deepEqual(rankShortcodeEntries(entries, ""), []);
  assert.equal(rankShortcodeEntries(entries, "jo").length, 1);
});

test("exact beats prefix beats substring, and custom beats standard", () => {
  const entries = [
    standard("tada", "🎉"),
    standard("tadaaa", "🎊"),
    standard("not_tada", "🥳"),
    custom("tada"),
  ];
  const ranked = rankShortcodeEntries(entries, "tada");
  assert.deepEqual(
    ranked.map((e) => [e.shortcode, e.custom]),
    [
      ["tada", true],
      ["tada", false],
      ["tadaaa", false],
      ["not_tada", false],
    ],
  );
});

test("ranking is stable and honours the limit", () => {
  const entries = ["ab1", "ab2", "ab3", "ab4"].map((s) => standard(s, "x"));
  assert.deepEqual(
    rankShortcodeEntries([...entries].reverse(), "ab", 2).map((e) => e.shortcode),
    ["ab1", "ab2"],
  );
});

// ── application ─────────────────────────────────────────────────────────────

test("applying a completion replaces exactly the shortcode run", () => {
  const query = shortcodeQueryAt("ship it :ta", 11)!;
  assert.deepEqual(applyShortcode("ship it :ta", query.start, query.end, "🎉", true), {
    text: "ship it 🎉 ",
    caret: 8 + "🎉 ".length,
  });

  const closed = completedShortcodeAt("ship it :tada: now", 14)!;
  assert.deepEqual(
    applyShortcode("ship it :tada: now", closed.start, closed.end, "🎉", false),
    { text: "ship it 🎉 now", caret: 8 + "🎉".length },
  );
});

// ── the generated table ─────────────────────────────────────────────────────

test("the generated table carries the shortcodes people actually type", () => {
  const byShortcode = new Map<string, string>();
  for (const emoji of STANDARD_EMOJI) {
    for (const shortcode of emoji.shortcodes) {
      assert.equal(byShortcode.has(shortcode), false, `${shortcode} is claimed twice`);
      assert.match(shortcode, /^[a-z0-9_+-]+$/, `${shortcode} is outside the alphabet`);
      byShortcode.set(shortcode, emoji.char);
    }
  }
  assert.equal(byShortcode.get("joy"), "😂");
  assert.equal(byShortcode.get("tada"), "🎉");
  assert.equal(byShortcode.get("100"), "💯");
  // Aliases are real: both of these are the same emoji.
  assert.equal(byShortcode.get("+1"), byShortcode.get("thumbsup"));
  // Every alias the table carries must be one the parser can actually type.
  for (const shortcode of byShortcode.keys()) {
    const typed = `:${shortcode}:`;
    assert.equal(completedShortcodeAt(typed, typed.length)?.name, shortcode);
  }
});
