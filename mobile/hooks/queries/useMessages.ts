// Message read + send + ingest hooks. Mirrors the read paths of
// `frontend/src/hooks/queries/useMessages.ts` — `get_channel_messages` and
// `get_dm_messages` both invoke envelope-ingest internally before reading
// the local DB, so a single call gives a fresh page.
//
// Pagination: `useMessages` is an infinite query over cursor pages. The
// cursor for each older page is derived from the PREVIOUS PAGE'S DATA
// (`getNextPageParam`), never seeded from an effect — desktop PR #958 fixed
// a bug where a cursor seeded from an effect read the previous
// conversation's cached pages and permanently capped history at 50. Deriving
// from page data makes that state unrepresentable: a new conversation key
// starts from a fresh first page.

import { useCallback } from "react";
import {
  useInfiniteQuery,
  useMutation,
  useQuery,
  useQueryClient,
  type InfiniteData,
} from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";

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
}

/** Mirror of `pollis_core::commands::messages::ThreadSummary`. */
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
  reply_to_id?: string | null;
  thread_id?: string | null;
  created_at: number;
  edited_at?: number;
  deleted_at?: number;
  /** Local-only optimistic stub flag. Replaced when `send_message` resolves. */
  pending?: boolean;
}

/** One decoded page: newest-first messages plus the cursor to the next
 *  (older) page, exactly as the Rust `MessagePage` reports it. */
export interface DecodedPage {
  messages: Message[];
  nextCursor: MessageCursor | null;
}

export type MessagesData = InfiniteData<DecodedPage, MessageCursor | null>;

function parseContent(raw: string | undefined): string {
  if (!raw) {
    return "";
  }
  // Desktop wraps attachments in a structured envelope; mobile doesn't
  // upload attachments yet, so for now we just strip that envelope down
  // to the text payload if it's present.
  if (raw.startsWith("{")) {
    try {
      const parsed = JSON.parse(raw);
      if (typeof parsed?._txt === "string") {
        return parsed._txt;
      }
    } catch {
      // Fall through — treat as plain text.
    }
  }
  return raw;
}

function transform(raw: RawChannelMessage): Message {
  return {
    id: raw.id,
    conversation_id: raw.conversation_id,
    sender_id: raw.sender_id,
    sender_username: raw.sender_username,
    content: parseContent(raw.content),
    reply_to_id: raw.reply_to_id ?? null,
    thread_id: raw.thread_id ?? null,
    created_at: new Date(raw.sent_at).getTime(),
    edited_at: raw.edited_at ? new Date(raw.edited_at).getTime() : undefined,
    deleted_at: raw.deleted_at ? new Date(raw.deleted_at).getTime() : undefined,
  };
}

/** Flatten pages (each newest-first) into one newest-first array, dropping
 *  duplicates — after new arrivals shift the timeline, a refetched page
 *  boundary can briefly overlap its neighbour. */
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

/**
 * Infinite query over a conversation's message history, newest page first.
 * `get_*_messages` runs envelope ingest itself before reading, so each
 * first-page fetch picks up newly delivered messages. Older pages load via
 * `fetchNextPage` (the chat list calls it from `onEndReached` on its
 * inverted list — which RN re-evaluates on content-size changes as well as
 * scroll, so a load that lands with no scroll offset left still advances;
 * desktop PR #958's second bug shape).
 */
export function useMessages(
  conversationId: string | null,
  kind: ConversationKind | null,
  opts?: { limit?: number },
) {
  const currentUser = useObserver(() => appStore.currentUser);
  const limit = opts?.limit ?? 50;

  return useInfiniteQuery({
    queryKey: messageQueryKeys.conversation(conversationId, kind),
    initialPageParam: null as MessageCursor | null,
    queryFn: async ({ pageParam }): Promise<DecodedPage> => {
      if (!conversationId || !kind || !currentUser) {
        return { messages: [], nextCursor: null };
      }
      const cmd =
        kind === "channel" ? "get_channel_messages" : "get_dm_messages";
      const args: Record<string, unknown> =
        kind === "channel"
          ? { userId: currentUser.id, channelId: conversationId, limit }
          : { userId: currentUser.id, dmChannelId: conversationId, limit };
      if (pageParam) {
        args.cursor = pageParam;
      }
      const page = await invoke<MessagePage>(cmd, args);
      return {
        messages: (page.messages ?? []).map(transform),
        nextCursor: page.next_cursor ?? null,
      };
    },
    getNextPageParam: (lastPage) => lastPage.nextCursor,
    enabled: !!(conversationId && kind && currentUser),
    staleTime: 1000 * 15,
  });
}

/** Immutably update every cached page's message array. */
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

/**
 * Send a text message. Optimistic: the new message is prepended to the
 * newest cached page with `pending: true`, then replaced with the
 * server-confirmed row on success (or rolled back on failure).
 */
export function useSendMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async (vars: {
      content: string;
      replyToId?: string;
      threadId?: string;
    }) => {
      if (!conversationId || !currentUser) {
        throw new Error("No active conversation");
      }
      const raw = await invoke<RawChannelMessage>("send_message", {
        conversationId,
        senderId: currentUser.id,
        content: vars.content,
        replyToId: vars.replyToId ?? null,
        threadId: vars.threadId ?? null,
        senderUsername: currentUser.username ?? null,
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
      const optimistic: Message = {
        id: optimisticId,
        conversation_id: conversationId,
        sender_id: currentUser.id,
        sender_username: currentUser.username,
        content: vars.content,
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
      // Thread replies also land optimistically in the open thread's list
      // (an array cache, unlike the paged conversation cache).
      if (vars.threadId) {
        queryClient.setQueryData<Message[]>(
          messageQueryKeys.thread(vars.threadId),
          (cache) => [...(cache ?? []), optimistic],
        );
      }
      return { previous, optimisticId, key, threadId: vars.threadId };
    },
    onSuccess: (confirmed, _vars, ctx) => {
      if (!ctx) {
        return;
      }
      queryClient.setQueryData<MessagesData>(ctx.key, (cache) =>
        mapPages(cache, (msgs) =>
          msgs.map((m) => (m.id === ctx.optimisticId ? confirmed : m)),
        ),
      );
      if (ctx.threadId) {
        queryClient.setQueryData<Message[]>(
          messageQueryKeys.thread(ctx.threadId),
          (cache) =>
            (cache ?? []).map((m) =>
              m.id === ctx.optimisticId ? confirmed : m,
            ),
        );
        // Reply-count chips on the parent rows.
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.threadSummaries(conversationId),
        });
      }
    },
    onError: (_e, _vars, ctx) => {
      if (!ctx) {
        return;
      }
      // Roll back the optimistic stubs on failure.
      if (ctx.previous) {
        queryClient.setQueryData(ctx.key, ctx.previous);
      }
      if (ctx.threadId) {
        queryClient.setQueryData<Message[]>(
          messageQueryKeys.thread(ctx.threadId),
          (cache) => (cache ?? []).filter((m) => m.id !== ctx.optimisticId),
        );
      }
    },
  });
}

/**
 * Replies in one thread, oldest-first — `read_thread_messages` is a local
 * read over already-ingested rows (the conversation fetch ingests) and
 * returns them chronologically, so unlike the conversation pages this is
 * NOT reversed. The root message is not included; the thread screen takes
 * it from the conversation cache.
 */
export function useThreadMessages(threadId: string | null) {
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
      return raw.map(transform);
    },
    enabled: !!threadId,
    staleTime: 1000 * 30,
  });
}

/** Reply-count summaries for a conversation, keyed by thread root id. */
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
    onError: (_e, _vars, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(ctx.key, ctx.previous);
      }
    },
  });
}

/**
 * Imperative ingest trigger — fire-and-forget. Called from `useFocusEffect`
 * in the chat screen when the screen mounts or refocuses, so the user sees
 * any messages delivered while the app was backgrounded.
 */
export function useIngestConversation() {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();

  return useCallback(
    async (conversationId: string, kind: ConversationKind) => {
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
        // Receipt frames ride the same envelope stream — refresh the
        // conversation's receipt map alongside the messages (event-driven;
        // the receipts query itself never polls).
        queryClient.invalidateQueries({
          queryKey: ["receipts", conversationId],
        });
        // Freshly ingested envelopes may be thread replies — refresh every
        // open thread and this conversation's reply-count chips.
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.threads,
        });
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.threadSummaries(conversationId),
        });
      } catch (e) {
        // Best-effort — ingest is advisory. The next refetch will retry.
        console.warn("[useIngestConversation] ingest failed:", e);
      }
    },
    [currentUser, queryClient],
  );
}
