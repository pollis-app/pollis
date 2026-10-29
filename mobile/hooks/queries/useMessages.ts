// Pages are local reads; envelope ingest runs separately and invalidates them.
// Each older page's cursor comes from the previous page's data, never from an
// effect, so a new conversation key always starts fresh (#958).

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
import { aliasAttachment } from "../../lib/media/viewer";
import {
  outgoingAttachments,
  type PickedAttachment,
} from "../../lib/attachments";
import {
  carryStubs,
  mapOutboxRows,
  settleSend,
  unsentFlags,
  withoutOutboxRows,
  type SendState,
} from "./pendingMessages";
import { parseContent } from "../../lib/messageContent";

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
  /** Our own message the durable outbox has not delivered yet (#1335). */
  send_state?: SendState;
}

interface OutboxReport {
  delivered: string[];
  failed: string[];
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
  /** How many consecutive placeholders this row stands for (`collapseUndecryptable`). */
  undecryptableCount?: number;
  /** Local-only optimistic stub flag. */
  pending?: boolean;
  /** The send failed — a stub's, or a stored unsent row's (#1335). */
  failed?: boolean;
  /** The core's id for this outgoing message (#1335), used by Retry / Discard. */
  outboxId?: string;
  /** False when the send failed before the core stored it: discard only. */
  retryable?: boolean;
}

/** Newest-first messages plus the cursor to the next (older) page. */
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
    ...unsentFlags(raw.id, raw.send_state),
  };
}

/** Flatten newest-first pages, dropping duplicates: after new arrivals a
 *  refetched page boundary can briefly overlap its neighbour. */
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
  /** Prefix covering every open thread (invalidation on ingest). */
  threads: ["messages", "thread"] as const,
  threadSummaries: (conversationId: string | null) =>
    ["messages", "thread-summaries", conversationId] as const,
};

export const lastMessageQueryKeys = {
  all: ["last-message"] as const,
  batch: (conversationIds: string[]) =>
    ["last-message", "batch", conversationIds.join(",")] as const,
};

/** Infinite query over a conversation's message history, newest page first. */
export function useMessages(
  conversationId: string | null,
  kind: ConversationKind | null,
  opts?: { limit?: number },
) {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();
  const limit = opts?.limit ?? 50;

  return useInfiniteQuery({
    queryKey: messageQueryKeys.conversation(conversationId, kind),
    initialPageParam: null as MessageCursor | null,
    queryFn: async ({ pageParam }): Promise<DecodedPage> => {
      if (!conversationId || !kind || !currentUser) {
        return { messages: [], nextCursor: null };
      }
      // Not `get_*`: those ingest from the DS first, which would hold the
      // first page on the network (and on MLS setup for a new channel).
      const cmd =
        kind === "channel" ? "read_channel_messages" : "read_dm_messages";
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
    },
    getNextPageParam: (lastPage) => lastPage.nextCursor,
    enabled: !!(conversationId && kind && currentUser),
    staleTime: 1000 * 15,
  });
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

/** A send that failed before the core stored it (#1335): nothing to retry, only discard. */
class NotQueuedError extends Error {}

// Keyed by the mutation's variables object, which TanStack passes to both
// `onMutate` and `mutationFn`, so the send can tag its stub with the core's id.
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
 * Send a message optimistically: a `pending` stub is prepended, then swapped
 * for the stored row or marked `failed`. The core stores the message before
 * any network work, so an unfinished send survives a restart (#1335).
 */
export function useSendMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
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
      // The core names the message first (#1335) so a refetch that brings in
      // the stored row drops the stub instead of showing it twice.
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
      // Attachments go to the core un-uploaded; the outbox uploads them, so a
      // kill mid-upload loses nothing (#1335). `content` is then the caption.
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
      const optimisticId = `pending-${Date.now()}`;
      stubIds.set(vars, optimisticId);
      const optimistic: Message = {
        id: optimisticId,
        conversation_id: conversationId,
        sender_id: currentUser.id,
        sender_username: currentUser.username,
        content: vars.content,
        attachments: (vars.attachments ?? []).map((att) => ({
          id: att.id,
          object_key: "",
          content_hash: "",
          sha256: "",
          key_scope: null,
          filename: att.name,
          content_type: att.mimeType,
          file_size: 0,
          uploaded_at: Date.now(),
          width: att.width,
          height: att.height,
          localPreviewUri: att.uri,
        })),
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
      // A search that ran while the stub was on screen cached "no matches".
      queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
      if (!ctx) {
        return;
      }
      // send_message's row carries no sender_username; keep the one the stub
      // had so the author label doesn't change when the send reconciles.
      const settled: Message = {
        ...confirmed,
        sender_username: confirmed.sender_username ?? currentUser?.username,
      };
      // Pending and confirmed attachments pair up by position (the envelope
      // keeps picked order), so an open viewer follows the swap.
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
      // A refetch that read the store before this row was written, and lands
      // after this swap, would drop it again: cancel any such in-flight read
      // (invalidate's default) and read once more now that the row is stored.
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
      // Mark failed rather than roll back: a send that silently vanishes is
      // worse than one labelled "failed".
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

/** Retry one unsent message under the same id, so it can never arrive twice (#1335). */
export function useRetryMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
  threadId?: string | null,
) {
  const queryClient = useQueryClient();
  return useMutation({
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

/**
 * Throw away one unsent message (#1335). A row that was ever sealed is first
 * withdrawn from everyone; the core refuses the discard if it cannot.
 */
export function useDiscardUnsent(
  conversationId: string | null,
  kind: ConversationKind | null,
  threadId?: string | null,
) {
  const queryClient = useQueryClient();
  return useMutation({
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

/**
 * Deliver whatever the durable outbox holds (#1335). Called on sign-in, resume
 * and realtime reconnect, never on a timer.
 */
export async function flushOutbox(
  queryClient: ReturnType<typeof useQueryClient>,
): Promise<void> {
  try {
    const report = await invoke<OutboxReport>("flush_outbox");
    if ((report?.delivered?.length ?? 0) + (report?.failed?.length ?? 0) > 0) {
      await queryClient.invalidateQueries({ queryKey: messageQueryKeys.all });
      await queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
    }
  } catch (e) {
    console.warn("[outbox] flush_outbox failed:", e);
  }
}

/** Replies in one thread, oldest-first; the root message is not included. */
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
      // The local store (and its search index) changed; cached hits are stale.
      queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
    },
    onError: (_e, _vars, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(ctx.key, ctx.previous);
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
      // The local store (and its search index) changed; cached hits are stale.
      queryClient.invalidateQueries({ queryKey: searchQueryKeys.all });
    },
    onError: (_e, _vars, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(ctx.key, ctx.previous);
      }
    },
  });
}

export interface IngestOptions {
  // Re-read the unread counts once the envelopes land (default true). The
  // live wake-up passes false: it bumps the badge itself and refetches after,
  // so a refetch landing before its bump cannot count the message twice.
  refreshUnread?: boolean;
}

/** Fire-and-forget envelope ingest for one conversation. */
export function useIngestConversation() {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();

  return useCallback(
    async (
      conversationId: string,
      kind: ConversationKind,
      opts: IngestOptions = {},
    ) => {
      if (!currentUser) {
        return;
      }
      try {
        if (kind === "channel") {
          await invoke("ingest_channel_envelopes", {
            userId: currentUser.id,
            channelId: conversationId,
          });
        } else {
          await invoke("ingest_dm_envelopes", {
            userId: currentUser.id,
            dmChannelId: conversationId,
          });
        }
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.conversation(conversationId, kind),
        });
        // Receipt frames ride the same envelope stream.
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
        if (opts.refreshUnread !== false) {
          refreshUnread(queryClient);
        }
      } catch (e) {
        console.warn("[useIngestConversation] ingest failed:", e);
      }
    },
    [currentUser, queryClient],
  );
}

export type LastMessageMap = Record<string, Message>;

/**
 * The newest local message for every conversation, in one bridge call
 * (#874/#936). A conversation with no local messages is absent from the map.
 */
export function useLastMessages(conversationIds: string[]) {
  const currentUser = useObserver(() => appStore.currentUser);

  // Sorted + de-duplicated so the cache key depends on the set, not render order.
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

/** One-line preview text for a conversation row. */
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
