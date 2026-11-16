/* Reply quotes (desktop parity): desktop's replyQuote.ts, copied verbatim, and its rule. */
import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { sentBeforeJoin } from "../lib/replyQuote.ts";

const here = dirname(fileURLToPath(import.meta.url));
const read = (p: string) => readFileSync(join(here, p), "utf8");

test("mobile/lib/replyQuote.ts is desktop's replyQuote.ts, byte for byte", () => {
  assert.equal(
    read("../lib/replyQuote.ts"),
    read("../../frontend/src/utils/replyQuote.ts"),
    "edit frontend/src/utils/replyQuote.ts and copy it to mobile/lib/replyQuote.ts",
  );
});

test("a quote of a message sent before joining says so; an unknown join never claims it", () => {
  const id = "01HQ7Z3K9M2P5R8T1V4W6Y0GEN";
  const sent = Date.parse("2024-02-22T08:32:26.932Z");
  assert.equal(sentBeforeJoin(id, sent + 1000), true);
  assert.equal(sentBeforeJoin(id, sent - 1000), false);
  assert.equal(sentBeforeJoin(id, null), false);
});

test("no screen says a message is 'encrypted': the copy names what happened", () => {
  const row = read("../components/chat/MessageRow.tsx");
  const chat = read("../app/chat/[id].tsx");
  assert.doesNotMatch(row + chat, /message\.encrypted/);
  assert.match(row, /message\.undecryptable\.title/);
  assert.match(row, /message\.beforeJoin/);
});
