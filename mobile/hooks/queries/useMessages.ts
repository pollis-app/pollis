// Pages are LOCAL reads; ingest runs off the render path. Cursors derive from page data, never an effect (desktop #958).

import { useCallback, useMemo } from "react";
import { reactionQueryKeys, type Reaction } from "./useReactions";
import { createMessageIdPool } from "../../lib/messageIds";
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
import type {
  LinkPreviewDraft,
  LinkPreviewWire,
  MessageAttachment,
  MessageLinkPreview,
} from "../../types";
import { searchQueryKeys } from "./useSearch";
import { refreshUnread } from "./useUnread";
import { keyChangeNoticeKeys } from "./useSafety";
import { bumpDMActivity, refreshDMActivity } from "./useDMChannels";
import { aliasAttachment } from "../../lib/media/viewer";
import {
  outgoingAttachments,
  type PickedAttachment,
} from "../../lib/attachments";
import { pickedToMessageAttachment } from "../../lib/composerAttachments";
import {
  carryStubs,
  applyLocalChanges,
  restoreRow,
  type LocalChange,
  mapOutboxRows,
  settleSend,
  withoutOutboxRows,
  type SendState,
} from "./pendingMessages";
import { optimisticLinkPreview } from "../../lib/messageContent";
import { flattenRows, shareMessagePagesById, transformMessage } from "./messageRows";
import { createSingleFlight } from "../../lib/singleFlight";
import { stripFormatting } from "../../../design/messageFormat.ts";

export type ConversationKind = "channel" | "dm";

export interface RawChannelMessage {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  /** #2085: the sender's display name when they set one; labels prefer it. */
  sender_display_name?: string | null;
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
  /** The sender's link preview (#1987), from the message's own trailer. */
  link_preview?: LinkPreviewWire | null;
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
  sender_display_name?: string | null;
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
  /** The DS refused it for good: the outbox will not try again. */
  rejected?: boolean;
  /** The core's id for this outgoing message (#1335). */
  outboxId?: string;
  /** False when the send failed before the core stored it: only discard. */
  retryable?: boolean;
  /** The sender's link preview (#1987); drawn only for a link in the body (`lib/linkPreview.ts`). */
  link_preview?: MessageLinkPreview | null;
}

export interface DecodedPage {
  messages: Message[];
  nextCursor: MessageCursor | null;
}

export type MessagesData = InfiniteData<DecodedPage, MessageCursor | null>;

/** Flatten newest-first pages, dropping duplicates where refetched page boundaries overlap. */
export const flattenPages = flattenRows;

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
  // Edits and deletes still in flight stay drawn over what the store holds (#2141).
  const messages = applyLocalChanges((page.messages ?? []).map(transformMessage), localChanges);
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
    // By message id across every page, never by index (#2168): an arrival slides each page.
    structuralSharing: shareMessagePagesById,
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
        structuralSharing: shareMessagePagesById,
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

/**
 * Edits and deletes drawn but not yet stored by the core (#2141), by the core's message id.
 * Every page read lays them over its rows (`applyLocalChanges`) until the change settles.
 */
const localChanges = new Map<string, LocalChange<Message>>();

/**
 * Draw `change` on message `id` and keep it drawn over refetches until it settles (#2141).
 * Returns the row as it was, for a refusal to put back.
 */
function drawLocalChange(
  queryClient: ReturnType<typeof useQueryClient>,
  key: readonly unknown[],
  id: string,
  change: LocalChange<Message>,
): Message | undefined {
  localChanges.set(id, change);
  let before: Message | undefined;
  queryClient.setQueryData<MessagesData>(key, (cache) =>
    mapPages(cache, (msgs) => {
      before ??= msgs.find((m) => m.id === id || m.outboxId === id);
      return applyLocalChanges(msgs, new Map([[id, change]]));
    }),
  );
  return before;
}

/** The change settled: stop drawing it, and put the one row back if it was refused. */
function settleLocalChange(
  queryClient: ReturnType<typeof useQueryClient>,
  key: readonly unknown[],
  id: string,
  change: LocalChange<Message>,
  refusedBefore: Message | undefined,
) {
  // A later change of the same message (a second edit) keeps its own.
  if (localChanges.get(id) === change) {
    localChanges.delete(id);
  }
  if (refusedBefore) {
    queryClient.setQueryData<MessagesData>(key, (cache) =>
      mapPages(cache, (msgs) => restoreRow(msgs, id, refusedBefore)),
    );
  }
}

/** Tells apart stubs made in the same millisecond. */
let stubSeq = 0;

/** Ids named before the tap (#2141): a stub carries the core's id from its first frame. */
export const messageIdPool = createMessageIdPool(() => invoke<string>("new_message_id"));

/** The id a send took from the pool, by its variables. */
const preNamed = new WeakMap<object, string>();

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
      // A finished composer preview (#1987); the core consumes its staged thumbnail.
      linkPreview?: LinkPreviewDraft | null;
    }) => {
      if (!conversationId || !currentUser) {
        throw new NotQueuedError("No active conversation");
      }
      // The core names the message first (#1335), so a refetch drops the stub instead of doubling
      // it. Usually named before the tap (#2141), and the stub already carries it.
      const named = preNamed.get(vars);
      const messageId =
        named ??
        (await invoke<string>("new_message_id").catch((e) => {
          throw new NotQueuedError(String(e));
        }));
      const stubId = stubIds.get(vars);
      if (!named && stubId && kind) {
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
        linkPreview: vars.linkPreview ?? null,
        replyToId: vars.replyToId ?? null,
        threadId: vars.threadId ?? null,
        senderUsername: currentUser.username ?? null,
        messageId,
      });
      return transformMessage(raw);
    },
    onMutate: async (vars) => {
      if (!conversationId || !kind || !currentUser) {
        return;
      }
      const key = messageQueryKeys.conversation(conversationId, kind);
      await queryClient.cancelQueries({ queryKey: key });
      // A DM moves to the top as the send starts (#1838); the stored row is kept even if it fails.
      if (kind === "dm") {
        bumpDMActivity(queryClient, currentUser.id, conversationId, Date.now());
      }
      stubSeq += 1;
      const optimisticId = `pending-${Date.now()}-${stubSeq}`;
      stubIds.set(vars, optimisticId);
      const named = messageIdPool.take();
      if (named) {
        preNamed.set(vars, named);
      }
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
        outboxId: named ?? undefined,
        link_preview: optimisticLinkPreview(vars.linkPreview),
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
      // The thumbnail uploads after the send (#1987): keep the composer's copy until it has.
      if (vars.linkPreview && !settled.link_preview?.image) {
        settled.link_preview = optimisticLinkPreview(vars.linkPreview);
      }
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
      // A just-sent row has no reactions (#2141): seed that, so nothing reads them from the DS.
      if (kind) {
        queryClient.setQueriesData<Map<string, Reaction[]>>(
          { queryKey: reactionQueryKeys.conversation(kind, conversationId) },
          (old) => (old === undefined || old.has(settled.id) ? old : new Map(old).set(settled.id, [])),
        );
      }
      // The patch above is the row (#2141); re-reading every loaded page would only repeat it.
      // Only a refetch still in flight may have read the store before this row was written,
      // and would put the stub back, so then read once more.
      if (queryClient.isFetching({ queryKey: ctx.key }) > 0) {
        queryClient.invalidateQueries({ queryKey: ctx.key });
      }
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
      transformMessage(await invoke<RawChannelMessage>("retry_message", { messageId: outboxId })),
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
    // Re-read whatever the report says. A flush that found the unlock drain
    // running waits for it, and an older core answers with only that drain's
    // empty last pass, so an empty report does not mean nothing was sent.
    // These are local reads, and every caller is already an event.
    await invoke<OutboxReport>("flush_outbox");
    await queryClient.invalidateQueries({ queryKey: messageQueryKeys.all });
    await queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
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
        raw.map(transformMessage),
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
    mutationFn: async (vars: {
      messageId: string;
      newContent: string;
      // An edit rebuilds the message (#1987): no preview here means none after it.
      linkPreview?: LinkPreviewDraft | null;
    }) => {
      if (!currentUser || !conversationId) {
        throw new Error("No active conversation");
      }
      await invoke("edit_message", {
        conversationId,
        messageId: vars.messageId,
        userId: currentUser.id,
        newContent: vars.newContent,
        linkPreview: vars.linkPreview ?? null,
      });
      return vars;
    },
    // Drawn on submit and kept over refetches until it settles (#2141): the core writes the row
    // only once the DS has the edit (#1555), so a refetch in between would read the old text.
    onMutate: (vars) => {
      if (!conversationId || !kind) {
        return;
      }
      const key = messageQueryKeys.conversation(conversationId, kind);
      const editedAt = Date.now();
      const preview = optimisticLinkPreview(vars.linkPreview);
      const change: LocalChange<Message> = (m) => ({
        ...m,
        content: vars.newContent,
        edited_at: editedAt,
        link_preview: preview,
      });
      const before = drawLocalChange(queryClient, key, vars.messageId, change);
      return { key, change, before };
    },
    onSuccess: (_data, vars, ctx) => {
      if (ctx) {
        settleLocalChange(queryClient, ctx.key, vars.messageId, ctx.change, undefined);
      }
      queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
    },
    onError: (_e, vars, ctx) => {
      if (!ctx) {
        return;
      }
      settleLocalChange(queryClient, ctx.key, vars.messageId, ctx.change, ctx.before);
      // The core kept the edit in its outbox (#1555); read it back.
      queryClient.invalidateQueries({ queryKey: ctx.key });
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
    // The row stays and reads "Message deleted", as the stored row will (desktop too), kept over
    // refetches until the delete settles (#2141). A refusal puts back that one row only, never
    // the whole page as it was (a send failing meanwhile keeps its Retry).
    onMutate: (messageId) => {
      if (!conversationId || !kind) {
        return;
      }
      const key = messageQueryKeys.conversation(conversationId, kind);
      const deletedAt = Date.now();
      const change: LocalChange<Message> = (m) => ({
        ...m,
        content: "",
        attachments: [],
        deleted_at: deletedAt,
      });
      const before = drawLocalChange(queryClient, key, messageId, change);
      return { key, change, before };
    },
    onSuccess: (_data, messageId, ctx) => {
      if (ctx) {
        settleLocalChange(queryClient, ctx.key, messageId, ctx.change, undefined);
      }
      queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
    },
    onError: (_e, messageId, ctx) => {
      if (ctx) {
        settleLocalChange(queryClient, ctx.key, messageId, ctx.change, ctx.before);
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
        // What the ingest stored moves its DM to the top at once (#1838); the persisted feed
        // covers rows stored by other paths (Android's push fetch).
        if (kind === "dm") {
          void refreshDMActivity(queryClient, userId, [conversationId]);
        }
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
        out[row.conversation_id] = transformMessage(row);
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
    return i18n.t("chat:message.deleted");
  }
  if (message.undecryptable) {
    return i18n.t("chat:preview.undecryptable");
  }
  if (message.content) {
    // The words without the formatting markers (#2142).
    return stripFormatting(message.content);
  }
  if (message.attachments && message.attachments.length > 0) {
    return i18n.t("mobile:direct.previewAttachment");
  }
  return null;
}
