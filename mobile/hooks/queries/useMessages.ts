// Pages are LOCAL reads; ingest runs off the render path. Cursors derive from page data, never an effect (desktop #958).

import { useCallback, useMemo } from "react";
import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
  type InfiniteData,
} from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import i18n from "../../i18n";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type { MessageAttachment } from "../../types";
import { searchQueryKeys } from "./useSearch";
import { refreshUnread } from "./useUnread";
import { keyChangeNoticeKeys } from "./useSafety";
import { aliasAttachment } from "../../lib/media/viewer";
import {
  outgoingAttachments,
  type PickedAttachment,
} from "../../lib/attachments";
import { pickedToMessageAttachment } from "../../lib/composerAttachments";
import {
  carryStubs,
  mapOutboxRows,
  settleSend,
  unsentFlags,
  withoutOutboxRows,
  type SendState,
} from "./pendingMessages";
import { parseContent } from "../../lib/messageContent";
import { createSingleFlight } from "../../lib/singleFlight";

export type ConversationKind = "channel" | "dm";

export interface RawChannelMessage {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  ciphertext?: string;
  content?: string;
  reply_to_id?: string | null;
  /** ULID of the thread root this message replies into (#825). */
  thread_id?: string | null;
  sent_at: string;
  edited_at?: string;
  deleted_at?: string;
  /** A placeholder for a message this device could not decrypt (#1312). */
  undecryptable?: boolean;
  /** Our own message the durable outbox has not delivered yet (#1335).
   *  Absent once delivered, and on everything received. */
  send_state?: SendState;
  edit_state?: SendState;
}

interface OutboxReport {
  delivered: string[];
  failed: string[];
  edits_delivered: string[];
  edits_failed: string[];
}

export interface ThreadSummary {
  thread_id: string;
  reply_count: number;
  last_reply_at?: string | null;
}

export interface MessageCursor {
  sent_at: string;
  id: string;
}

export interface MessagePage {
  messages: RawChannelMessage[];
  next_cursor: MessageCursor | null;
}

export interface Message {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  content: string;
  attachments: MessageAttachment[];
  reply_to_id?: string | null;
  thread_id?: string | null;
  created_at: number;
  edited_at?: number;
  deleted_at?: number;
  /** A placeholder for a message this device could not decrypt (#1312). */
  undecryptable?: boolean;
  /** Our edit is stored but its delivery failed (#1555). */
  edit_unsent?: boolean;
  undecryptableCount?: number;
  pending?: boolean;
  failed?: boolean;
  /** The core's id for this outgoing message (#1335). */
  outboxId?: string;
  /** False when the send failed before the core stored it: only discard. */
  retryable?: boolean;
}

export interface DecodedPage {
  messages: Message[];
  nextCursor: MessageCursor | null;
}

export type MessagesData = InfiniteData<DecodedPage, MessageCursor | null>;

function transform(raw: RawChannelMessage): Message {
  const parsed = parseContent(raw.content, { conversationId: raw.conversation_id });
  return {
    id: raw.id,
    conversation_id: raw.conversation_id,
    sender_id: raw.sender_id,
    sender_username: raw.sender_username,
    content: parsed.text,
    attachments: parsed.attachments,
    reply_to_id: raw.reply_to_id ?? null,
    thread_id: raw.thread_id ?? null,
    created_at: new Date(raw.sent_at).getTime(),
    edited_at: raw.edited_at ? new Date(raw.edited_at).getTime() : undefined,
    deleted_at: raw.deleted_at ? new Date(raw.deleted_at).getTime() : undefined,
    undecryptable: raw.undecryptable === true ? true : undefined,
    edit_unsent:
      raw.edit_state === "failed" || raw.edit_state === "rejected" ? true : undefined,
    ...unsentFlags(raw.id, raw.send_state),
  };
}

/** Flatten newest-first pages, dropping duplicates where refetched page boundaries overlap. */
export function flattenPages(
  data: { pages: DecodedPage[] } | undefined,
): Message[] {
  if (!data) {
    return [];
  }
  const seen = new Set<string>();
  const out: Message[] = [];
  for (const page of data.pages) {
    for (const m of page.messages) {
      if (seen.has(m.id)) {
        continue;
      }
      seen.add(m.id);
      out.push(m);
    }
  }
  return out;
}

export const messageQueryKeys = {
  all: ["messages"] as const,
  conversation: (
    conversationId: string | null,
    kind: ConversationKind | null,
  ) => ["messages", kind, conversationId] as const,
  thread: (threadId: string | null) =>
    ["messages", "thread", threadId] as const,
  threads: ["messages", "thread"] as const,
  threadSummaries: (conversationId: string | null) =>
    ["messages", "thread-summaries", conversationId] as const,
};

export const lastMessageQueryKeys = {
  all: ["last-message"] as const,
  batch: (conversationIds: string[]) =>
    ["last-message", "batch", conversationIds.join(",")] as const,
};

/** Newest page size, and how long a page read from the local store counts as fresh. */
const PAGE_LIMIT = 50;
const MESSAGES_STALE_MS = 1000 * 15;

type QueryClient = ReturnType<typeof useQueryClient>;

/** One page of a conversation, read from the local store only; ingest runs separately. */
async function readMessagePage(
  queryClient: QueryClient,
  conversationId: string,
  kind: ConversationKind,
  limit: number,
  pageParam: MessageCursor | null,
): Promise<DecodedPage> {
  const cmd = kind === "channel" ? "read_channel_messages" : "read_dm_messages";
  const args: Record<string, unknown> =
    kind === "channel"
      ? { channelId: conversationId, limit }
      : { dmChannelId: conversationId, limit };
  if (pageParam) {
    args.cursor = pageParam;
  }
  const page = await invoke<MessagePage>(cmd, args);
  const messages = (page.messages ?? []).map(transform);
  // A refetch of the newest page while a send is in flight must not drop
  // its optimistic stub, which is never in the store (pendingMessages.ts).
  const newest = pageParam
    ? messages
    : carryStubs(
        messages,
        queryClient.getQueryData<MessagesData>(
          messageQueryKeys.conversation(conversationId, kind),
        )?.pages[0]?.messages,
      );
  return {
    messages: newest,
    nextCursor: page.next_cursor ?? null,
  };
}

/** Infinite query over a conversation's history, newest page first. */
export function useMessages(
  conversationId: string | null,
  kind: ConversationKind | null,
  opts?: { limit?: number },
) {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();
  const limit = opts?.limit ?? PAGE_LIMIT;

  return useInfiniteQuery({
    queryKey: messageQueryKeys.conversation(conversationId, kind),
    initialPageParam: null as MessageCursor | null,
    queryFn: async ({ pageParam }): Promise<DecodedPage> => {
      if (!conversationId || !kind || !currentUser) {
        return { messages: [], nextCursor: null };
      }
      return readMessagePage(queryClient, conversationId, kind, limit, pageParam);
    },
    getNextPageParam: (lastPage) => lastPage.nextCursor,
    enabled: !!(conversationId && kind && currentUser),
    staleTime: MESSAGES_STALE_MS,
  });
}

/**
 * Read a conversation's newest page into the cache before it opens (#1610), from a row's touch
 * down: the local read is done by the time the screen mounts, so what the store holds is on its
 * first frame instead of a loading line. A no-op while the cached page is fresh.
 */
export function usePrefetchConversation() {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();
  return useCallback(
    (conversationId: string, kind: ConversationKind) => {
      if (!currentUser) {
        return;
      }
      void queryClient.prefetchInfiniteQuery({
        queryKey: messageQueryKeys.conversation(conversationId, kind),
        initialPageParam: null as MessageCursor | null,
        queryFn: ({ pageParam }) =>
          readMessagePage(queryClient, conversationId, kind, PAGE_LIMIT, pageParam),
        getNextPageParam: (lastPage: DecodedPage) => lastPage.nextCursor,
        staleTime: MESSAGES_STALE_MS,
      });
    },
    [currentUser, queryClient],
  );
}

/**
 * Re-read conversations whose local rows just changed (#1610): every cached page of each,
 * including conversations not on screen, so opening one later shows it on the first frame.
 * Local reads only.
 */
export function refreshStoredConversations(queryClient: QueryClient, conversationIds: string[]) {
  if (conversationIds.length === 0) {
    return;
  }
  const ids = new Set(conversationIds);
  void queryClient.invalidateQueries({
    predicate: (q) =>
      q.queryKey[0] === "messages" &&
      (q.queryKey[1] === "channel" || q.queryKey[1] === "dm") &&
      ids.has(q.queryKey[2] as string),
    refetchType: "all",
  });
  void queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
  refreshUnread(queryClient);
}

function mapPages(
  cache: MessagesData | undefined,
  fn: (messages: Message[]) => Message[],
): MessagesData | undefined {
  if (!cache) {
    return cache;
  }
  return {
    ...cache,
    pages: cache.pages.map((p) => ({ ...p, messages: fn(p.messages) })),
  };
}

/** A send that failed before the core stored it (#1335): only discardable. */
class NotQueuedError extends Error {}

/** The stub id `onMutate` chose, keyed by the mutation's variables object. */
const stubIds = new WeakMap<object, string>();

function patchOutboxRows(
  queryClient: ReturnType<typeof useQueryClient>,
  key: readonly unknown[],
  threadId: string | null | undefined,
  outboxId: string,
  fn: (m: Message) => Message,
) {
  queryClient.setQueryData<MessagesData>(key, (cache) =>
    mapPages(cache, (msgs) => mapOutboxRows(msgs, outboxId, fn)),
  );
  if (threadId) {
    queryClient.setQueryData<Message[]>(messageQueryKeys.thread(threadId), (cache) =>
      cache ? mapOutboxRows(cache, outboxId, fn) : cache,
    );
  }
}

/** Tells apart stubs made in the same millisecond. */
let stubSeq = 0;

/**
 * Send a text message optimistically; durable via the core's outbox (#1335).
 *
 * Never blocks the composer (#1636): `mutate` may be called again while earlier sends are in
 * flight. The core stores each one at once and delivers a conversation's sends one at a time,
 * in the order it stored them, so nothing here waits or orders.
 */
export function useSendMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { mutate: "send" } },
    mutationFn: async (vars: {
      content: string;
      replyToId?: string;
      threadId?: string;
      attachments?: PickedAttachment[];
    }) => {
      if (!conversationId || !currentUser) {
        throw new NotQueuedError("No active conversation");
      }
      // The core names the message first (#1335), so a refetch drops the stub instead of doubling it.
      const messageId = await invoke<string>("new_message_id").catch((e) => {
        throw new NotQueuedError(String(e));
      });
      const stubId = stubIds.get(vars);
      if (stubId && kind) {
        const key = messageQueryKeys.conversation(conversationId, kind);
        const tag = (m: Message): Message => (m.id === stubId ? { ...m, outboxId: messageId } : m);
        queryClient.setQueryData<MessagesData>(key, (cache) =>
          mapPages(cache, (msgs) => msgs.map(tag)),
        );
        if (vars.threadId) {
          queryClient.setQueryData<Message[]>(
            messageQueryKeys.thread(vars.threadId),
            (cache) => (cache ?? []).map(tag),
          );
        }
      }
      // Attachments go to the core un-uploaded (#1335); the outbox uploads them.
      const raw = await invoke<RawChannelMessage>("send_message", {
        conversationId,
        senderId: currentUser.id,
        content: vars.content,
        attachments: outgoingAttachments(vars.attachments ?? []),
        replyToId: vars.replyToId ?? null,
        threadId: vars.threadId ?? null,
        senderUsername: currentUser.username ?? null,
        messageId,
      });
      return transform(raw);
    },
    onMutate: async (vars) => {
      if (!conversationId || !kind || !currentUser) {
        return;
      }
      const key = messageQueryKeys.conversation(conversationId, kind);
      await queryClient.cancelQueries({ queryKey: key });
      stubSeq += 1;
      const optimisticId = `pending-${Date.now()}-${stubSeq}`;
      stubIds.set(vars, optimisticId);
      const optimistic: Message = {
        id: optimisticId,
        conversation_id: conversationId,
        sender_id: currentUser.id,
        sender_username: currentUser.username,
        content: vars.content,
        attachments: (vars.attachments ?? []).map((att) =>
          pickedToMessageAttachment(att, Date.now()),
        ),
        reply_to_id: vars.replyToId ?? null,
        thread_id: vars.threadId ?? null,
        created_at: Date.now(),
        pending: true,
      };
      const previous = queryClient.getQueryData<MessagesData>(key);
      if (previous && previous.pages.length > 0) {
        const pages = previous.pages.slice();
        pages[0] = {
          ...pages[0],
          messages: [optimistic, ...pages[0].messages],
        };
        queryClient.setQueryData<MessagesData>(key, {
          ...previous,
          pages,
        });
      } else {
        queryClient.setQueryData<MessagesData>(key, {
          pages: [{ messages: [optimistic], nextCursor: null }],
          pageParams: [null],
        });
      }
      if (vars.threadId) {
        queryClient.setQueryData<Message[]>(
          messageQueryKeys.thread(vars.threadId),
          (cache) => [...(cache ?? []), optimistic],
        );
      }
      return { previous, optimisticId, key, threadId: vars.threadId };
    },
    onSuccess: (confirmed, vars, ctx) => {
      // A search that ran before the row was stored cached "no matches".
      queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
      if (!ctx) {
        return;
      }
      // send_message's row carries no sender_username; keep the stub's.
      const settled: Message = {
        ...confirmed,
        sender_username: confirmed.sender_username ?? currentUser?.username,
      };
      (vars.attachments ?? []).forEach((att, i) => {
        const to = confirmed.attachments?.[i]?.id;
        if (to) {
          aliasAttachment(att.id, to);
        }
      });
      queryClient.setQueryData<MessagesData>(ctx.key, (cache) =>
        cache
          ? {
              ...cache,
              pages: cache.pages.map((p, i) =>
                i === 0
                  ? { ...p, messages: settleSend(p.messages, ctx.optimisticId, settled) }
                  : p,
              ),
            }
          : cache,
      );
      // Re-read: a refetch that read the store before this row was written would drop it.
      queryClient.invalidateQueries({ queryKey: ctx.key });
      if (ctx.threadId) {
        queryClient.setQueryData<Message[]>(
          messageQueryKeys.thread(ctx.threadId),
          (cache) => settleSend(cache ?? [], ctx.optimisticId, settled, "oldestFirst"),
        );
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.threadSummaries(conversationId),
        });
      }
    },
    onError: (e, _vars, ctx) => {
      if (!ctx) {
        return;
      }
      // Keep the stub marked failed: a send that silently vanishes is worse.
      const retryable = !(e instanceof NotQueuedError);
      const markFailed = (m: Message): Message =>
        m.id === ctx.optimisticId ? { ...m, pending: false, failed: true, retryable } : m;
      queryClient.setQueryData<MessagesData>(ctx.key, (cache) =>
        mapPages(cache, (msgs) => msgs.map(markFailed)),
      );
      if (ctx.threadId) {
        queryClient.setQueryData<Message[]>(
          messageQueryKeys.thread(ctx.threadId),
          (cache) => (cache ?? []).map(markFailed),
        );
      }
      if (retryable) {
        queryClient.invalidateQueries({ queryKey: ctx.key });
        if (ctx.threadId) {
          queryClient.invalidateQueries({ queryKey: messageQueryKeys.thread(ctx.threadId) });
        }
      }
    },
  });
}

/** Retry one unsent message (#1335) under the same id, so it never arrives twice. */
export function useRetryMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
  threadId?: string | null,
) {
  const queryClient = useQueryClient();
  return useMutation({
    // A retry feels like the send it repeats (#1286).
    meta: { haptic: { mutate: "send" } },
    mutationFn: async (outboxId: string) =>
      transform(await invoke<RawChannelMessage>("retry_message", { messageId: outboxId })),
    onMutate: (outboxId) => {
      if (!conversationId || !kind) {
        return;
      }
      patchOutboxRows(
        queryClient,
        messageQueryKeys.conversation(conversationId, kind),
        threadId,
        outboxId,
        (m) => ({ ...m, pending: true, failed: false }),
      );
    },
    onError: (_e, outboxId) => {
      if (!conversationId || !kind) {
        return;
      }
      patchOutboxRows(
        queryClient,
        messageQueryKeys.conversation(conversationId, kind),
        threadId,
        outboxId,
        (m) => ({ ...m, pending: false, failed: true }),
      );
    },
    onSettled: () => {
      if (!conversationId || !kind) {
        return;
      }
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.conversation(conversationId, kind),
      });
      queryClient.invalidateQueries({ queryKey: messageQueryKeys.threads });
      queryClient.invalidateQueries({ queryKey: messageQueryKeys.threadSummaries(conversationId) });
      queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
    },
  });
}

/** Discard one unsent message (#1335); the core withdraws it first if it was ever sealed. */
export function useDiscardUnsent(
  conversationId: string | null,
  kind: ConversationKind | null,
  threadId?: string | null,
) {
  const queryClient = useQueryClient();
  return useMutation({
    // Throwing a message away feels like deleting one (#1286).
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (outboxId: string) =>
      invoke<boolean>("discard_unsent_message", { messageId: outboxId }),
    onMutate: (outboxId) => {
      if (!conversationId || !kind) {
        return;
      }
      queryClient.setQueryData<MessagesData>(
        messageQueryKeys.conversation(conversationId, kind),
        (cache) => mapPages(cache, (msgs) => withoutOutboxRows(msgs, outboxId)),
      );
      if (threadId) {
        queryClient.setQueryData<Message[]>(messageQueryKeys.thread(threadId), (cache) =>
          cache ? withoutOutboxRows(cache, outboxId) : cache,
        );
      }
    },
    onSettled: () => {
      if (!conversationId || !kind) {
        return;
      }
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.conversation(conversationId, kind),
      });
      queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
    },
  });
}

/** Deliver whatever the durable outbox holds (#1335). Event-driven, never on a timer. */
export async function flushOutbox(
  queryClient: ReturnType<typeof useQueryClient>,
): Promise<void> {
  try {
    const report = await invoke<OutboxReport>("flush_outbox");
    const touched =
      (report?.delivered?.length ?? 0) +
      (report?.failed?.length ?? 0) +
      (report?.edits_delivered?.length ?? 0) +
      (report?.edits_failed?.length ?? 0);
    if (touched > 0) {
      await queryClient.invalidateQueries({ queryKey: messageQueryKeys.all });
      await queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
    }
  } catch (e) {
    console.warn("[outbox] flush_outbox failed:", e);
  }
}

/** Replies in one thread, oldest-first (NOT reversed), root excluded. */
export function useThreadMessages(threadId: string | null) {
  const queryClient = useQueryClient();
  return useQuery({
    queryKey: messageQueryKeys.thread(threadId),
    queryFn: async (): Promise<Message[]> => {
      if (!threadId) {
        return [];
      }
      const raw =
        (await invoke<RawChannelMessage[]>("read_thread_messages", {
          threadId,
        })) ?? [];
      // Same race as the conversation pages: keep an in-flight reply's stub.
      return carryStubs(
        raw.map(transform),
        queryClient.getQueryData<Message[]>(messageQueryKeys.thread(threadId)),
        "oldestFirst",
      );
    },
    enabled: !!threadId,
    staleTime: 1000 * 30,
  });
}

export function useThreadSummaries(conversationId: string | null) {
  return useQuery({
    queryKey: messageQueryKeys.threadSummaries(conversationId),
    queryFn: async (): Promise<Map<string, ThreadSummary>> => {
      const map = new Map<string, ThreadSummary>();
      if (!conversationId) {
        return map;
      }
      const rows =
        (await invoke<ThreadSummary[]>("list_thread_summaries", {
          conversationId,
        })) ?? [];
      for (const row of rows) {
        map.set(row.thread_id, row);
      }
      return map;
    },
    enabled: !!conversationId,
    staleTime: 1000 * 30,
  });
}

export function useEditMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "success" } },
    mutationFn: async (vars: { messageId: string; newContent: string }) => {
      if (!currentUser || !conversationId) {
        throw new Error("No active conversation");
      }
      await invoke("edit_message", {
        conversationId,
        messageId: vars.messageId,
        userId: currentUser.id,
        newContent: vars.newContent,
      });
      return vars;
    },
    onMutate: async (vars) => {
      if (!conversationId || !kind) {
        return;
      }
      const key = messageQueryKeys.conversation(conversationId, kind);
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<MessagesData>(key);
      queryClient.setQueryData<MessagesData>(key, (cache) =>
        mapPages(cache, (msgs) =>
          msgs.map((m) =>
            m.id === vars.messageId
              ? { ...m, content: vars.newContent, edited_at: Date.now() }
              : m,
          ),
        ),
      );
      return { previous, key };
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
    },
    onError: (_e, _vars, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(ctx.key, ctx.previous);
      }
      // The core kept the edit in its outbox (#1555); read it back.
      if (ctx?.key) {
        queryClient.invalidateQueries({ queryKey: ctx.key });
      }
    },
  });
}

export function useDeleteMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    // Tactile feedback (#1286): lib/queryClient.ts plays it.
    meta: { haptic: { success: "destructive" } },
    mutationFn: async (messageId: string) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      await invoke("delete_message", {
        messageId,
        userId: currentUser.id,
      });
      return messageId;
    },
    onMutate: async (messageId) => {
      if (!conversationId || !kind) {
        return;
      }
      const key = messageQueryKeys.conversation(conversationId, kind);
      await queryClient.cancelQueries({ queryKey: key });
      const previous = queryClient.getQueryData<MessagesData>(key);
      queryClient.setQueryData<MessagesData>(key, (cache) =>
        mapPages(cache, (msgs) => msgs.filter((m) => m.id !== messageId)),
      );
      return { previous, key };
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
    },
    onError: (_e, _vars, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(ctx.key, ctx.previous);
      }
    },
  });
}

/** One ingest per conversation at a time; requests mid-run share one trailing run (#1610). */
const ingestFlights = createSingleFlight();

/** Fire-and-forget ingest, run when the chat screen focuses. */
export function useIngestConversation() {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();

  return useCallback(
    async (
      conversationId: string,
      kind: ConversationKind,
    ) => {
      if (!currentUser) {
        return;
      }
      try {
        const userId = currentUser.id;
        // Keyed by account too: a trailing run must never reuse another account's closure.
        await ingestFlights.run(`${userId}:${kind}:${conversationId}`, async () => {
          if (kind === "channel") {
            await invoke("ingest_channel_envelopes", {
              userId,
              channelId: conversationId,
            });
          } else {
            await invoke("ingest_dm_envelopes", {
              userId,
              dmChannelId: conversationId,
            });
          }
        });
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.conversation(conversationId, kind),
        });
        queryClient.invalidateQueries({
          queryKey: ["receipts", conversationId],
        });
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.threads,
        });
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.threadSummaries(conversationId),
        });
        queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
        queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
        // Any ingest may have re-pinned a changed key (#1605); the notices are local, so cheap.
        queryClient.invalidateQueries({ queryKey: keyChangeNoticeKeys.all });
        // Badges come from Rust's counts after every ingest, never a local bump (#1628).
        refreshUnread(queryClient);
      } catch (e) {
        console.warn("[useIngestConversation] ingest failed:", e);
      }
    },
    [currentUser, queryClient],
  );
}


export type LastMessageMap = Record<string, Message>;

/** Newest local message per conversation in ONE bridge call (#874/#936); no entry = no preview. */
export function useLastMessages(conversationIds: string[]) {
  const currentUser = useObserver(() => appStore.currentUser);

  const idsKey = conversationIds.join(",");
  const ids = useMemo(
    () => Array.from(new Set(conversationIds)).sort(),
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [idsKey],
  );

  return useQuery({
    queryKey: lastMessageQueryKeys.batch(ids),
    queryFn: async (): Promise<LastMessageMap> => {
      if (ids.length === 0) {
        return {};
      }
      const rows = await invoke<RawChannelMessage[]>("read_last_messages", {
        conversationIds: ids,
      });
      const out: LastMessageMap = {};
      for (const row of rows ?? []) {
        out[row.conversation_id] = transform(row);
      }
      return out;
    },
    enabled: ids.length > 0 && !!currentUser,
    staleTime: 1000 * 30,
  });
}

export function previewText(message: Message | undefined): string | null {
  if (!message) {
    return null;
  }
  if (message.deleted_at) {
    return i18n.t("mobile:direct.previewDeleted");
  }
  if (message.undecryptable) {
    return i18n.t("chat:preview.undecryptable");
  }
  if (message.content) {
    return message.content;
  }
  if (message.attachments && message.attachments.length > 0) {
    return i18n.t("mobile:direct.previewAttachment");
  }
  return null;
}
