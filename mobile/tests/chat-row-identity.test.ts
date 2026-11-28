/* The rows React renders (#2168), counted on the real list pipeline. MessageRowItem is `memo`
 * with React's default shallow compare (pinned below), so a row renders again exactly when one
 * of the props `messageRowProps` builds changes identity, and a new key mounts. `rowRenders`
 * counts exactly that over the code the screen runs: the stored rows decoded
 * (`transformMessage`), a re-read shared by id (`shareMessagePagesById`), the list items
 * (`buildChatItems`) and each row's props (`messageRowProps`). Desktop's twin is the incoming
 * case in e2e/render-cost.spec.ts (#2167). */

import test from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { replaceEqualDeep } from "@tanstack/react-query";
import { flattenRows, shareMessagePagesById, transformMessage } from "../hooks/queries/messageRows.ts";
import { buildChatItems, type ChatListItem } from "../lib/chatItems.ts";
import { messageRowProps, type RowView } from "../components/chat/messageRowProps.ts";
import { mergeReactions } from "../hooks/queries/reactionBatch.ts";
import { retryTarget, unsentReason } from "../hooks/queries/pendingMessages.ts";
import type { MessagesData, RawChannelMessage } from "../hooks/queries/useMessages";
import type { Reaction } from "../hooks/queries/useReactions";

const src = (p: string) => readFileSync(new URL(`../${p}`, import.meta.url), "utf8");
const chat = src("app/chat/[id].tsx");
const renderItem = chat.slice(chat.indexOf("const renderItem = useCallback("), chat.indexOf("const content = ("));

const PAGE = 50;
const T0 = Date.UTC(2026, 7, 1, 10, 0, 0);
const idOf = (i: number) => `01J${String(i).padStart(10, "0")}`;

function raw(i: number, extra: Partial<RawChannelMessage> = {}): RawChannelMessage {
  // Every fifth row carries an attachment, so a fresh per-read value in it would show.
  const content =
    i % 5 === 0
      ? JSON.stringify({
          _att: [{ key: `k${i}`, hash: `h${i}`, kv: 1, sha: "ab", name: "a.png", ct: "image/png", size: 1234, w: 100, h: 80 }],
          _txt: `caption ${i}`,
        })
      : `message body ${i}`;
  return {
    id: idOf(i),
    conversation_id: "conv-1",
    sender_id: `user-${Math.floor(i / 3) % 2}`,
    sender_username: `user${Math.floor(i / 3) % 2}`,
    content,
    sent_at: new Date(T0 + i * 60_000).toISOString(),
    ...extra,
  };
}

/** The core's reads: newest page first, each cut by cursor from the one before (the sliding window). */
function pages(rows: RawChannelMessage[], pageCount: number): MessagesData {
  const newestFirst = [...rows].reverse().map(transformMessage);
  const out: MessagesData = { pages: [], pageParams: [] };
  for (let p = 0; p < pageCount; p++) {
    const slice = newestFirst.slice(p * PAGE, (p + 1) * PAGE);
    const last = slice[slice.length - 1];
    out.pages.push({ messages: slice, nextCursor: last ? { sent_at: String(last.created_at), id: last.id } : null });
    out.pageParams.push(p === 0 ? null : out.pages[p - 1].nextCursor);
  }
  return out;
}

const LABELS = {
  dayKey: (ts: number) => new Date(ts).toDateString(),
  dayLabel: (ts: number) => new Date(ts).toDateString(),
};

// Built once per screen with useMemo: the same objects across every render here.
const CTX = {} as RowView["ctx"];
const HANDLERS = {} as RowView["handlers"];

function view(over: Partial<RowView> = {}): RowView {
  return {
    ctx: CTX,
    handlers: HANDLERS,
    reactionsByMessage: undefined,
    receiptsByMessage: undefined,
    receiptMessageId: null,
    threadSummaries: undefined,
    repliedTo: new Map(),
    highlightId: null,
    railPeer: null,
    readUpToMessageId: null,
    readUpToLabel: (name) => `read by ${name}`,
    entering: () => undefined,
    ...over,
  };
}

type Props = ReturnType<typeof messageRowProps>;

function rowProps(data: MessagesData, v: RowView): Map<string, Props> {
  const items: ChatListItem[] = buildChatItems(flattenRows(data), [], false, LABELS);
  const out = new Map<string, Props>();
  for (const item of items) {
    if (item.type === "msg") {
      out.set(item.key, messageRowProps(item.message, item.continued, v));
    }
  }
  return out;
}

/** React.memo's default compare: every prop by Object.is, plus a mount for a new key. */
function rowRenders(before: Map<string, Props>, after: Map<string, Props>): string[] {
  const rendered: string[] = [];
  for (const [key, props] of after) {
    const old = before.get(key);
    if (!old) {
      rendered.push(key);
      continue;
    }
    const names = new Set([...Object.keys(old), ...Object.keys(props)]);
    for (const k of names) {
      if (!Object.is((old as Record<string, unknown>)[k], (props as Record<string, unknown>)[k])) {
        rendered.push(key);
        break;
      }
    }
  }
  return rendered;
}

const share = (a: MessagesData, b: MessagesData) => shareMessagePagesById(a, b) as MessagesData;
const allRows = (d: MessagesData) => d.pages.flatMap((p) => p.messages);

test("MessageRowItem keeps React's default shallow compare, and takes exactly messageRowProps", () => {
  const item = src("components/chat/MessageRowItem.tsx");
  const body = item.slice(item.indexOf("export const MessageRowItem = memo("));
  assert.match(body, /^export const MessageRowItem = memo\(function MessageRowItem\(/);
  // memo(Component) with no second argument: no comparator that could skip a field.
  assert.match(body, /\n\}\);\s*$/);
  assert.ok(renderItem.includes("{...messageRowProps(m, item.continued, rowView)}"));
});

test("a keystroke re-renders no row: nothing a row takes moves while typing", () => {
  const data = pages(Array.from({ length: 120 }, (_, i) => raw(i)), 2);
  const v = view();
  assert.deepEqual(rowRenders(rowProps(data, v), rowProps(data, v)), []);
  // The draft reaches no list input.
  const deps = renderItem.slice(renderItem.lastIndexOf("["));
  assert.ok(!/\bdraft\b/.test(deps), "renderItem must not depend on the draft");
  const rowView = chat.slice(chat.indexOf("const rowView = useMemo<RowView>("), chat.indexOf("const renderItem = useCallback("));
  assert.ok(rowView.length > 0 && !/\bdraft\b/.test(rowView), "rowView must not depend on the draft");
  // The FlatList's own props keep identity across a render and it runs in strictMode, so a
  // keystroke's screen render skips the list and its cells entirely.
  const list = chat.slice(chat.indexOf("<FlatList\n"), chat.indexOf("</ReadRailProvider>"));
  assert.ok(list.length > 0);
  assert.match(list, /\n\s+strictMode\n/);
  assert.match(list, /keyExtractor=\{chatItemKey\}/);
  assert.match(list, /style=\{listRevealed \? LIST_SHOWN : LIST_HIDDEN\}/);
  assert.match(list, /contentContainerStyle=\{LIST_CONTENT\}/);
  assert.match(list, /ListFooterComponent=\{listFooter\}/);
  assert.ok(!/=\{\(/.test(list), "no inline function prop on the FlatList");
  assert.ok(!/=\{\{/.test(list), "no inline object prop on the FlatList");
});

test("one arrival into a busy, paged conversation renders only the new row", () => {
  const rows = Array.from({ length: 120 }, (_, i) => raw(i));
  const v = view();
  const cached = pages(rows, 3);
  const before = rowProps(cached, v);
  // The newest page slides by one, so every row of every page moves.
  const fresh = pages([...rows, raw(120)], 3);
  const rendered = rowRenders(before, rowProps(share(cached, fresh), v));
  assert.ok(rendered.length <= 2, `rendered ${rendered.length}: ${rendered.join(",")}`);
  assert.ok(rendered.includes(idOf(120)));
  // React Query's default (index) sharing re-renders every mounted row: what this guards.
  const byIndex = replaceEqualDeep(cached, pages([...rows, raw(120)], 3)) as MessagesData;
  assert.ok(rowRenders(before, rowProps(byIndex, v)).length >= 100);
});

test("an unchanged re-read keeps every object, down to the page set", () => {
  const rows = Array.from({ length: 70 }, (_, i) => raw(i));
  const cached = pages(rows, 2);
  assert.equal(share(cached, pages(rows, 2)), cached);
});

test("a row whose own data changed always gets a new object, and renders", () => {
  const rows = Array.from({ length: 60 }, (_, i) => raw(i));
  const target = 57;
  const id = idOf(target);
  const cases: [string, Partial<RawChannelMessage>, Partial<RawChannelMessage>][] = [
    ["pending -> failed", { send_state: "pending" }, { send_state: "failed" }],
    ["failed -> pending (retry)", { send_state: "failed" }, { send_state: "pending" }],
    ["pending -> delivered", { send_state: "pending" }, {}],
    ["failed -> rejected", { send_state: "failed" }, { send_state: "rejected" }],
    ["edit not sent", {}, { edit_state: "failed", edited_at: "2026-08-01T12:00:00.000Z" }],
    ["edited", {}, { content: "edited body", edited_at: "2026-08-01T12:00:00.000Z" }],
    ["deleted", {}, { deleted_at: "2026-08-01T12:00:00.000Z", content: "" }],
    ["undecryptable -> decrypted", { undecryptable: true, content: "" }, {}],
  ];
  for (const [name, was, now] of cases) {
    const cached = pages(rows.map((r, i) => (i === target ? raw(i, was) : r)), 2);
    const next = share(cached, pages(rows.map((r, i) => (i === target ? raw(i, now) : r)), 2));
    const oldRow = allRows(cached).find((m) => m.id === id);
    const newRow = allRows(next).find((m) => m.id === id);
    assert.notEqual(newRow, oldRow, `${name}: a new object`);
    assert.deepEqual(newRow, transformMessage(raw(target, now)), `${name}: exactly what was read`);
    const cachedById = new Map(allRows(cached).map((m) => [m.id, m]));
    assert.ok(
      allRows(next).filter((m) => m.id !== id).every((m) => cachedById.get(m.id) === m),
      `${name}: every other row keeps its object`,
    );
    const v = view();
    const rendered = rowRenders(rowProps(cached, v), rowProps(next, v));
    assert.ok(rendered.includes(id), `${name}: the row renders`);
    assert.ok(rendered.length <= 2, `${name}: rendered ${rendered.length}`);
  }
});

test("a send that turns failed re-renders its row, with Retry and Discard", () => {
  const rows = Array.from({ length: 60 }, (_, i) => raw(i));
  const id = idOf(59);
  const cached = pages(rows.map((r, i) => (i === 59 ? raw(i, { send_state: "pending" }) : r)), 2);
  const next = share(cached, pages(rows.map((r, i) => (i === 59 ? raw(i, { send_state: "failed" }) : r)), 2));
  const v = view();
  const after = rowProps(next, v);
  assert.deepEqual(rowRenders(rowProps(cached, v), after), [id]);
  const m = after.get(id)!.message;
  assert.equal(m.failed, true);
  assert.equal(retryTarget(m), id);
  assert.equal(unsentReason(m), "willResend");
  // MessageRowItem derives Retry from retryTarget and Discard from `failed` on every render.
  const item = src("components/chat/MessageRowItem.tsx");
  assert.match(item, /onRetry=\{retryId \? \(\) => handlers\.onRetry\(retryId\) : undefined\}/);
  assert.match(item, /onDiscard=\{m\.failed \? \(\) => handlers\.onDiscard\(coreId\) : undefined\}/);
});

test("a row that slides to the next page keeps its object; two copies of one id are never merged", () => {
  const rows = Array.from({ length: 100 }, (_, i) => raw(i));
  const cached = pages(rows, 2);
  const next = share(cached, pages([...rows, raw(100)], 2));
  // Row 50 was on page 0 and is now on page 1.
  const id = idOf(50);
  assert.equal(
    next.pages[1].messages.find((m) => m.id === id),
    cached.pages[0].messages.find((m) => m.id === id),
  );
  // A refetch that briefly shows one id on two pages, the copies differing: each page keeps its own.
  const fresh = transformMessage(raw(50, { send_state: "failed" }));
  const base = pages(rows, 2);
  const dup: MessagesData = {
    pages: [
      { ...base.pages[0], messages: base.pages[0].messages.map((m) => (m.id === id ? fresh : m)) },
      { ...base.pages[1], messages: [transformMessage(raw(50)), ...base.pages[1].messages] },
    ],
    pageParams: base.pageParams,
  };
  const shared = share(cached, dup);
  assert.equal(shared.pages[0].messages.find((m) => m.id === id)?.failed, true);
  assert.equal(shared.pages[1].messages[0].failed, undefined);
});

test("not a page set: the default sharing", () => {
  const a = { foo: [1, 2] };
  assert.equal(shareMessagePagesById(a, { foo: [1, 2] }), a);
  assert.equal(shareMessagePagesById(undefined, { pages: [], pageParams: [] }) !== undefined, true);
});

test("a re-read of a received attachment reference equals the last one", () => {
  const a = transformMessage(raw(5));
  assert.equal(a.attachments[0].uploaded_at, 0);
  assert.equal(replaceEqualDeep(a, transformMessage(raw(5))), a);
});

test("a reaction re-read renders only the row whose reactions changed", () => {
  const rows = Array.from({ length: 60 }, (_, i) => raw(i));
  const data = pages(rows, 2);
  const ids = rows.map((r) => r.id);
  const read = (bump: string | null) =>
    new Map<string, Reaction[]>(
      ids.map((id) => [id, id === bump ? [{ emoji: "👍", user_ids: ["u1"], count: 1 }] : []]),
    );
  const first = mergeReactions(undefined, read(null));
  // A full re-read (a realtime reaction event) with one row changed.
  const second = mergeReactions(first, read(ids[10]));
  const rendered = rowRenders(
    rowProps(data, view({ reactionsByMessage: first })),
    rowProps(data, view({ reactionsByMessage: second })),
  );
  assert.deepEqual(rendered, [ids[10]]);
});

test("the highlight renders only the row it lands on and the one it leaves", () => {
  const rows = Array.from({ length: 60 }, (_, i) => raw(i));
  const data = pages(rows, 2);
  const a = rowProps(data, view({ highlightId: rows[3].id }));
  const b = rowProps(data, view({ highlightId: rows[7].id }));
  assert.deepEqual(rowRenders(a, b).sort(), [rows[3].id, rows[7].id].sort());
});

test("nothing writes into a cached Message in place (identity reuse would hide the change)", () => {
  // A shared row is reused by identity, so an in-place write would make the old object equal the
  // new read and the row would never render the change. Every cache writer spreads instead.
  const files = [
    "hooks/queries/useMessages.ts",
    "hooks/queries/pendingMessages.ts",
    "hooks/queries/messageRows.ts",
    "app/chat/[id].tsx",
    "app/chat/thread.tsx",
    "lib/chatItems.ts",
  ];
  const fields = [
    "content", "failed", "pending", "rejected", "retryable", "outboxId", "edited_at", "deleted_at",
    "edit_unsent", "attachments", "link_preview", "undecryptable", "undecryptableCount",
    "created_at", "reply_to_id", "thread_id",
  ].join("|");
  // (A row being built before it is cached, such as a settled send's `settled`, may be filled in.)
  const write = new RegExp(`\\b(?:m|msg|message|row|stub|old|prev|cached)\\.(?:${fields})\\s*(?:=(?!=)|\\+=|-=|\\+\\+|--)`);
  for (const f of files) {
    src(f).split("\n").forEach((line, i) => {
      assert.ok(!write.test(line), `${f}:${i + 1} writes a Message field in place: ${line.trim()}`);
    });
  }
});
