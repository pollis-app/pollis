/* Chat render cost (#2141): a keystroke or a reaction toggle re-renders no row but its own,
 * a settling send keeps its row, and a send does not re-read every loaded page. The device
 * tier (`.maestro/flows/chat-perf.yaml`) counts the renders; these pin the code that makes it so.
 * Desktop's twin is e2e/render-cost.spec.ts (#874). */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");

const chat = src("app/chat/[id].tsx");
const renderItem = chat.slice(chat.indexOf("const renderItem = useCallback("), chat.indexOf("const content = ("));

test("rows are memoised and take stable handlers, never per-render closures", () => {
  const item = src("components/chat/MessageRowItem.tsx");
  assert.match(item, /export const MessageRowItem = memo\(/);
  assert.ok(renderItem.includes("<MessageRowItem"), "the conversation draws MessageRowItem");
  assert.ok(!renderItem.includes("<MessageRow\n"), "not the bare, unmemoised row");
  // The mutation results are new objects every render: renderItem must not depend on them.
  const deps = renderItem.slice(renderItem.lastIndexOf("["));
  for (const unstable of ["toggleReaction", "retryMessage", "discardUnsent", "currentUser,"]) {
    assert.ok(!deps.includes(unstable), `renderItem must not depend on ${unstable}`);
  }
  // The handlers are built once and read the latest state through a ref.
  assert.match(chat, /const rowHandlers = useMemo<RowHandlers>\([\s\S]*?\[\],\n  \);/);
});

test("a row still re-renders when its own data changes", () => {
  // The per-row props are the message, its reactions entry and its receipt, so a changed
  // message, a toggled reaction (a new array for that id only) or a new receipt reach it.
  for (const prop of ["message={m}", "reactions={reactionsByMessage?.get(m.id)}", "receipt={"]) {
    assert.ok(renderItem.includes(prop), `renderItem passes ${prop}`);
  }
});

test("a stub and its stored row share one key", () => {
  assert.match(chat, /key: m\.outboxId \?\? m\.id/);
});

test("the thread's rows keep their callbacks across keystrokes", () => {
  const thread = src("app/chat/thread.tsx");
  assert.match(thread, /const \{ mutate: retrySend \} = useRetryMessage/);
  assert.match(thread, /const \{ mutate: discardSend \} = useDiscardUnsent/);
});

test("a settled send patches its row instead of re-reading every loaded page", () => {
  const hook = src("hooks/queries/useMessages.ts");
  const send = hook.slice(hook.indexOf("export function useSendMessage"), hook.indexOf("export function useRetryMessage"));
  const success = send.slice(send.indexOf("onSuccess:"), send.indexOf("onError:"));
  assert.match(success, /if \(queryClient\.isFetching\(\{ queryKey: ctx\.key \}\) > 0\) \{\n\s+queryClient\.invalidateQueries/);
  assert.equal(success.match(/invalidateQueries\(\{ queryKey: ctx\.key \}\)/g)?.length, 1);
  // And seeds the new row's empty reactions.
  assert.match(success, /new Map\(old\)\.set\(settled\.id, \[\]\)/);
});
