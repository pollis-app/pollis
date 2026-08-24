/*
 * The composer's contentEditable is only safe if serialization is exact.
 *
 * `pollis-core::commands::emoji::parse_emoji_tokens` reads whatever this
 * produces, so "what the editor sends" has to be byte-identical to what the
 * `<textarea>` used to send for the same logical content. The rules under test:
 *
 *   - token -> node -> token is lossless, for every position a token can be in;
 *   - a malformed near-token (`<:oops:>`) is TEXT and survives as those exact
 *     characters — the same rule `emojiTokens.ts` states for rendering;
 *   - an element's rendered contents never reach the wire; only its validated
 *     `data-emoji-token` does, so a forged attribute cannot smuggle markup;
 *   - the terminal skin's ghost is a rendering and contributes nothing;
 *   - a paste is `text/plain` and nothing else, so no amount of pasted HTML can
 *     put a `<`, `>` or `&` into the message that was not typed as one.
 *
 * DOM-free on purpose: `frontend/tests/` runs on Node's built-in runner with no
 * browser, so the serializer is written against the handful of `Node` members
 * it touches and the fixtures below are plain objects.
 *
 *   node --test frontend/tests/
 */

import test from "node:test";
import assert from "node:assert/strict";

import {
  EMOJI_TOKEN_ATTR,
  GHOST_ATTR,
  emojiTokenAfter,
  emojiTokenBefore,
  pastedPlainText,
  richRuns,
  serializeRichInput,
  type SerializableNode,
} from "../src/components/ui/richInputModel.ts";
import { splitEmojiSegments } from "../src/components/Emoji/emojiTokens.ts";

const PARROT = "<:partyparrot:d7da905542ed745664126a7722b588eef0634737afe8f7a2b9fb7699f13fd3bd>";
const SHIPIT = "<:shipit:a1b2c3d4e5f60718293a4b5c6d7e8f90a1b2c3d4e5f60718293a4b5c6d7e8f90>";

// ── minimal DOM fixtures ───────────────────────────────────────────────────

function text(value: string): SerializableNode {
  return { nodeType: 3, nodeName: "#text", textContent: value, childNodes: [] };
}

function element(
  nodeName: string,
  attributes: Record<string, string>,
  children: SerializableNode[] = [],
): SerializableNode {
  return {
    nodeType: 1,
    nodeName,
    textContent: null,
    childNodes: children,
    getAttribute: (name) => (name in attributes ? attributes[name] : null),
  };
}

function root(children: SerializableNode[]): SerializableNode {
  return { nodeType: 1, nodeName: "DIV", textContent: null, childNodes: children };
}

/** What `project()` builds for one custom emoji: the token in an attribute. */
function emojiNode(token: string, shortcode: string): SerializableNode {
  return element(
    "SPAN",
    { [EMOJI_TOKEN_ATTR]: token, "data-shortcode": shortcode, contenteditable: "false" },
    [element("IMG", { alt: `:${shortcode}:` })],
  );
}

/**
 * The projection `RichTextInput` builds, expressed against the fixtures.
 *
 * Kept in step with `project()` by construction: both are driven by `richRuns`,
 * so a change to how the value is split shows up on both sides at once.
 */
function project(value: string): SerializableNode {
  const children: SerializableNode[] = [];
  for (const run of richRuns(value)) {
    if (run.kind === "text") {
      children.push(text(run.text));
      continue;
    }
    children.push(emojiNode(run.token, run.shortcode));
  }
  if (value === "" || value.endsWith("\n")) {
    children.push(element("BR", {}));
  }
  return root(children);
}

// ── round-trip ─────────────────────────────────────────────────────────────

const ROUND_TRIP_CASES = [
  "",
  "plain text",
  PARROT,
  `hello${PARROT} world`,
  `${PARROT}${SHIPIT}`,
  `${PARROT} `,
  ` ${PARROT}`,
  `line one\nline ${PARROT} two`,
  "trailing newline\n",
  "two\n\nblank",
  // A malformed near-token, which the grammar says is ordinary text.
  "<:oops:> and <:x:1234> and <::>",
  // Standard Unicode emoji are plain characters and stay plain characters.
  "\u{1F602} party \u{1F389}",
  // A shortcode that was typed but never resolved.
  ":partyparrot: is not a token",
  // The delimiters as literal prose.
  "a < b > c & d",
  `${PARROT}<:oops:>${SHIPIT}`,
];

test("token -> node -> token is lossless", () => {
  for (const value of ROUND_TRIP_CASES) {
    assert.equal(
      serializeRichInput(project(value)),
      value,
      `round trip changed ${JSON.stringify(value)}`,
    );
  }
});

test("the projection carries exactly the tokens the renderer would draw", () => {
  // `richRuns` and `splitEmojiSegments` must agree about what a token is, or
  // the composer would show an image for something a message body renders as
  // text (or the reverse).
  for (const value of ROUND_TRIP_CASES) {
    const projected = richRuns(value)
      .filter((run) => run.kind === "emoji")
      .map((run) => run.token);
    const rendered = splitEmojiSegments(value)
      .filter((segment) => segment.kind === "emoji")
      .map((segment) => `<:${segment.shortcode}:${segment.contentHash}>`);
    assert.deepEqual(projected, rendered, `disagreement on ${JSON.stringify(value)}`);
  }
});

test("a malformed near-token stays literal characters, not an emoji node", () => {
  const value = "<:oops:>";
  assert.deepEqual(richRuns(value), [{ kind: "text", text: "<:oops:>" }]);
  assert.equal(serializeRichInput(project(value)), value);
});

// ── nothing but text and validated tokens reaches the wire ─────────────────

test("an emoji node contributes its token, never its rendered contents", () => {
  // The image is a rendering. If serialization ever read the subtree instead of
  // the attribute, a fallback-to-`:shortcode:` node would send the wrong thing.
  const node = root([
    emojiNode(PARROT, "partyparrot"),
    // The failure rendering: same attribute, text contents instead of an image.
    element("SPAN", { [EMOJI_TOKEN_ATTR]: SHIPIT }, [text(":shipit:")]),
  ]);
  assert.equal(serializeRichInput(node), `${PARROT}${SHIPIT}`);
});

test("a forged data-emoji-token is read as ordinary markup, not as a token", () => {
  // The attribute is validated against the grammar rather than trusted, so an
  // element that claims a token it cannot have contributes only its text.
  const forged = root([
    element("SPAN", { [EMOJI_TOKEN_ATTR]: "<:x:></span><script>alert(1)</script>" }, [
      text("harmless"),
    ]),
  ]);
  assert.equal(serializeRichInput(forged), "harmless");

  // Uppercase hex and an over-long shortcode are both outside the grammar.
  const badHash = "<:ok:" + "A".repeat(64) + ">";
  assert.equal(
    serializeRichInput(root([element("SPAN", { [EMOJI_TOKEN_ATTR]: badHash }, [text("x")])])),
    "x",
  );
});

test("pasted rich markup serializes to its text and nothing else", () => {
  // What a browser would have inserted if the paste were not intercepted: a
  // tree of styled elements, links and an image. None of the markup may appear.
  const pasted = root([
    element("DIV", {}, [
      element("B", {}, [text("bold")]),
      text(" and "),
      element("A", { href: "https://example.com" }, [text("a link")]),
    ]),
    element("DIV", {}, [
      element("IMG", { src: "x", onerror: "alert(1)" }),
      element("SPAN", { style: "color:red" }, [text("styled")]),
    ]),
  ]);
  const out = serializeRichInput(pasted);
  assert.equal(out, "bold and a link\nstyled");
  // Every character and attribute value that only markup could have supplied.
  // ("style" itself is not in the list — the word appears in the pasted TEXT.)
  for (const forbidden of ["<", ">", "&", "https://example.com", "alert(1)", "color:red"]) {
    assert.ok(!out.includes(forbidden), `serialized output leaked ${forbidden}`);
  }
});

test("the ghost is a rendering and contributes nothing", () => {
  const withGhost = root([
    text("hey @da"),
    element("SPAN", { [GHOST_ATTR]: "", "data-testid": "mention-ghost" }, [text("na")]),
  ]);
  assert.equal(serializeRichInput(withGhost), "hey @da");
});

test("a trailing <br> is the editing host's filler, an inner one is a newline", () => {
  assert.equal(serializeRichInput(root([text("a"), element("BR", {})])), "a");
  assert.equal(
    serializeRichInput(root([text("a"), element("BR", {}), text("b")])),
    "a\nb",
  );
  // An emptied editable, which every engine leaves holding one filler.
  assert.equal(serializeRichInput(root([element("BR", {})])), "");
});

// ── atomic deletion ────────────────────────────────────────────────────────

test("backspace and forward-delete find the whole token, never one character", () => {
  const value = `hi${PARROT}there`;
  const start = 2;
  const end = start + PARROT.length;

  assert.deepEqual(emojiTokenBefore(value, end), { start, end });
  assert.deepEqual(emojiTokenAfter(value, start), { start, end });

  // Anywhere else there is no token adjacent, so the browser's own deletion
  // stands — one character, as it should be.
  assert.equal(emojiTokenBefore(value, 1), null);
  assert.equal(emojiTokenBefore(value, end + 3), null);
  assert.equal(emojiTokenAfter(value, 0), null);
  assert.equal(emojiTokenAfter(value, end), null);

  // Adjacent tokens: each is found on its own boundary.
  const pair = `${PARROT}${SHIPIT}`;
  assert.deepEqual(emojiTokenBefore(pair, PARROT.length), {
    start: 0,
    end: PARROT.length,
  });
  assert.deepEqual(emojiTokenAfter(pair, PARROT.length), {
    start: PARROT.length,
    end: pair.length,
  });
});

test("a near-token is not atomically deletable — it is just characters", () => {
  const value = "x<:oops:>y";
  for (let caret = 0; caret <= value.length; caret += 1) {
    assert.equal(emojiTokenBefore(value, caret), null);
    assert.equal(emojiTokenAfter(value, caret), null);
  }
});

// ── paste sanitization ─────────────────────────────────────────────────────

/** A clipboard that records which flavours were asked for. */
function clipboard(flavours: Record<string, string>) {
  const asked: string[] = [];
  return {
    asked,
    data: {
      getData: (type: string) => {
        asked.push(type);
        return flavours[type] ?? "";
      },
    },
  };
}

test("a paste reads text/plain and never text/html", () => {
  const board = clipboard({
    "text/plain": "hello",
    "text/html": '<img src=x onerror="alert(1)"><b>hello</b>',
  });
  assert.equal(pastedPlainText(board.data), "hello");
  assert.ok(
    !board.asked.includes("text/html"),
    "the html flavour was read; the sanitizer must never parse markup",
  );
});

test("a pasted raw token survives verbatim and becomes an emoji node", () => {
  const board = clipboard({ "text/plain": `look ${PARROT} here` });
  const pasted = pastedPlainText(board.data);
  assert.equal(pasted, `look ${PARROT} here`);
  // Spliced into the model, it projects as an image — the whole point.
  assert.deepEqual(
    richRuns(pasted).map((run) => run.kind),
    ["text", "emoji", "text"],
  );
});

test("a paste of literal markup stays literal characters", () => {
  // Typed `<` and `>` are ordinary text and must survive as themselves — the
  // grammar is what makes that safe, since no token can contain them.
  const board = clipboard({ "text/plain": "<script>alert(1)</script> & <:oops:>" });
  assert.equal(pastedPlainText(board.data), "<script>alert(1)</script> & <:oops:>");
});

test("a paste normalizes CRLF and drops control characters", () => {
  // Tab and newline are things a person can type, so they stay. A bare CR from
  // a Windows app would otherwise travel over the wire, and a stray NUL or ESC
  // would be invisible in the composer and present in the message.
  const board = clipboard({ "text/plain": "a\r\nb\rc\td\u0000e\u001Bf" });
  assert.equal(pastedPlainText(board.data), "a\nb\nc\tdef");
});

test("an empty clipboard is empty, not a crash", () => {
  const board = clipboard({});
  assert.equal(pastedPlainText(board.data), "");
});
