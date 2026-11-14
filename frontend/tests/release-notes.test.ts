/* Release notes on the Update page (#1916): changelog text becomes text blocks, never markup. */

import test from "node:test";
import assert from "node:assert/strict";

import { parseNoteSpans, parseReleaseNotes } from "../src/utils/releaseNotes.ts";

const plain = (text: string) => ({ text, style: "plain" as const });
const bold = (text: string) => ({ text, style: "bold" as const });
const code = (text: string) => ({ text, style: "code" as const });

test("a changelog entry is a list, with wrapped lines joined to their bullet", () => {
  const notes = [
    "- **Notifications do not tell Apple which conversation they",
    "  are for.** A push carries a single random handle.",
    "- Deleting a message requires proof.",
  ].join("\n");
  assert.deepEqual(parseReleaseNotes(notes), [
    {
      kind: "list",
      items: [
        [
          bold("Notifications do not tell Apple which conversation they are for."),
          plain(" A push carries a single random handle."),
        ],
        [plain("Deleting a message requires proof.")],
      ],
    },
  ]);
});

test("paragraphs, headings and lists come out in order", () => {
  const notes = "These releases shipped together.\nSecond line.\n\n### Calls\n\n- One\n\n- Two\n\nClosing words.";
  assert.deepEqual(parseReleaseNotes(notes), [
    { kind: "paragraph", spans: [plain("These releases shipped together. Second line.")] },
    { kind: "heading", spans: [plain("Calls")] },
    { kind: "list", items: [[plain("One")], [plain("Two")]] },
    { kind: "paragraph", spans: [plain("Closing words.")] },
  ]);
});

test("a line after a blank line does not join the bullet above", () => {
  assert.deepEqual(parseReleaseNotes("- One\n\nNot part of it."), [
    { kind: "list", items: [[plain("One")]] },
    { kind: "paragraph", spans: [plain("Not part of it.")] },
  ]);
});

test("bold, code and autolinks are the only inline forms", () => {
  assert.deepEqual(parseNoteSpans("Run `pollis --version`, then see <https://pollis.com/artifacts>."), [
    plain("Run "),
    code("pollis --version"),
    plain(", then see https://pollis.com/artifacts."),
  ]);
  assert.deepEqual(parseNoteSpans("**All bold**"), [bold("All bold")]);
  // Unclosed markers are just characters.
  assert.deepEqual(parseNoteSpans("2 ** 3 and a ` tick"), [plain("2 ** 3 and a ` tick")]);
});

test("markup and links stay literal text", () => {
  const notes = '- <img src=x onerror="alert(1)"> and [a link](https://example.com)\n<script>alert(1)</script>';
  assert.deepEqual(parseReleaseNotes(notes), [
    {
      kind: "list",
      items: [
        [plain('<img src=x onerror="alert(1)"> and [a link](https://example.com) <script>alert(1)</script>')],
      ],
    },
  ]);
  // Only http(s) addresses lose their angle brackets; nothing becomes a link either way.
  assert.deepEqual(parseNoteSpans("<javascript:alert(1)>"), [plain("<javascript:alert(1)>")]);
});

test("empty notes are no blocks", () => {
  assert.deepEqual(parseReleaseNotes(""), []);
  assert.deepEqual(parseReleaseNotes("\n  \r\n"), []);
});
