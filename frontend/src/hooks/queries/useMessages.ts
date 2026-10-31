import { useEffect, useMemo, useRef } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
import { unreadQueryKeys } from "./useUnread";
import { useObserver } from "mobx-react-lite";
import type { AttachmentKeyScope, ContentOrigin, Message, DMConversation } from "../../types";
import type { OutgoingAttachment } from "../../utils/attachmentEnvelope";
import {
  carryStubs,
  mapOutboxRows,
  outboxFields,
  settleSend,
  withoutOutboxRows,
  type SendState,
} from "../../utils/pendingMessages";
import { createSendLine, type SendTurn } from "../../utils/sendLine";

// Last background ingest per conversation, to debounce channel switching (realtime writes it too).
const lastIngestAt = new Map<string, number>();
const INGEST_DEBOUNCE_MS = 5_000;

export function shouldIngest(conversationId: string): boolean {
  const last = lastIngestAt.get(conversationId) ?? 0;
  return performance.now() - last >= INGEST_DEBOUNCE_MS;
}

export function markIngested(conversationId: string): void {
  lastIngestAt.set(conversationId, performance.now());
}

export const messageQueryKeys = {
  all: ["messages"] as const,
  channel: (channelId: string | null) => ["messages", "channel", channelId] as const,
  conversation: (conversationId: string | null) => ["messages", "conversation", conversationId] as const,
  dmConversations: (userId: string | null) => ["dm-conversations", userId] as const,
  thread: (threadId: string | null) => ["messages", "thread", threadId] as const,
  /** Prefix covering every open thread — neither `channel` nor `conversation` reaches it. */
  threads: ["messages", "thread"] as const,
  threadSummaries: (conversationId: string | null) =>
    ["messages", "thread-summaries", conversationId] as const,
};

export const lastMessageQueryKeys = {
  all: ["last-message"] as const,
  // One entry per SET of conversations (#874); invalidate via `all`.
  batch: (conversationIds: string[]) =>
    ["last-message", "batch", conversationIds.join(",")] as const,
};

interface AttachmentWire {
  key: string;
  /** The object's random file id (#1382). */
  hash: string;
  /** Which key of the conversation's file key ring sealed it; `0` in the Vault. */
  kv?: number;
  /** Plaintext SHA-256, verified after decrypt. */
  sha?: string;
  name: string;
  ct: string;
  size: number;
  bh?: string;
  w?: number;
  h?: number;
  /** Sensitive media (#1568); omitted unless set. */
  sens?: boolean;
}

type RawMessage = {
  id: string;
  conversation_id: string;
  sender_id: string;
  content?: string;
  reply_to_id?: string;
  /** ULID of the thread root this was sent into (#825). */
  thread_id?: string;
  sent_at: string;
};

export type RawChannelMessage = {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  ciphertext: string;
  content?: string;
  reply_to_id?: string;
  /** ULID of the thread root, recovered from inside the ciphertext (#825). */
  thread_id?: string;
  sent_at: string;
  edited_at?: string;
  deleted_at?: string;
  /** A placeholder for a message this device could not decrypt (#1312). */
  undecryptable?: boolean;
  /** Our own message the durable outbox has not delivered yet (#1335). */
  send_state?: SendState;
  /** Our edit of it the DS has not confirmed yet (#1555); `content` is the edit. */
  edit_state?: SendState;
};

/** Mirror of `pollis_core::commands::messages::OutboxReport`. */
type OutboxReport = {
  delivered: string[];
  failed: string[];
  edits_delivered: string[];
  edits_failed: string[];
};

type MessagePage = {
  messages: RawChannelMessage[];
  next_cursor: { sent_at: string; id: string } | null;
};

type RawDmChannel = {
  id: string;
  created_by: string;
  created_at: string;
  members: Array<{ user_id: string; username?: string; avatar_url?: string; added_by: string; added_at: string }>;
};

// Decode the content string: plain text, or `{"_att":[…],"_txt":caption}`. Shared by pins (#99) and the Vault (#107); `origin` names each attachment's key (#1382).
export function parseContent(
  raw: string | undefined,
  origin: ContentOrigin,
): { text: string; attachments: Message['attachments'] } {
  if (!raw?.startsWith('{')) {
    return { text: raw ?? '', attachments: [] };
  }
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed._att)) {
      return { text: raw, attachments: [] };
    }
    return {
      text: parsed._txt ?? '',
      // No `key` yet: not uploaded by the outbox (#1335), drawn pending.
      attachments: (parsed._att as AttachmentWire[]).map((a, i) => ({
        id: a.key ?? `uploading-${i}`,
        object_key: a.key ?? '',
        content_hash: a.hash ?? '',
        sha256: a.sha ?? '',
        key_scope: keyScopeFor(a, origin),
        filename: a.name,
        content_type: a.ct,
        file_size: a.size,
        uploaded_at: Date.now(),
        blurhash: a.bh,
        width: a.w,
        height: a.h,
        sensitive: a.sens === true,
      })),
    };
  } catch {
    return { text: raw, attachments: [] };
  }
}

/** The attachment's key, or null if it can't be opened (uploading, or pre-#1382). */
function keyScopeFor(a: AttachmentWire, origin: ContentOrigin): AttachmentKeyScope | null {
  if (!a.key || !a.sha || typeof a.kv !== 'number') {
    return null;
  }
  if (origin === 'vault') {
    return a.kv === 0 ? { kind: 'vault' } : null;
  }
  return a.kv >= 1
    ? { kind: 'conversation', conversation_id: origin.conversationId, key_version: a.kv }
    : null;
}

function transformMessage(m: RawMessage): Message {
  // Undefined content (decryption failure) stays undefined so MessageList shows [encrypted].
  const parsed = m.content !== undefined ? parseContent(m.content, { conversationId: m.conversation_id }) : null;
  return {
    id: m.id,
    channel_id: undefined,
    conversation_id: m.conversation_id,
    sender_id: m.sender_id,
    ciphertext: new Uint8Array(),
    nonce: new Uint8Array(),
    content_decrypted: parsed?.text,
    reply_to_message_id: m.reply_to_id,
    thread_id: m.thread_id,
    is_pinned: false,
    created_at: new Date(m.sent_at).getTime(),
    delivered: true,
    status: 'sent' as const,
    attachments: parsed?.attachments ?? [],
  };
}

export function transformChannelMessage(m: RawChannelMessage): Message {
  // Undefined content (decryption failure) stays undefined so MessageList shows [encrypted].
  const parsed = m.content !== undefined ? parseContent(m.content, { conversationId: m.conversation_id }) : null;
  return {
    id: m.id,
    channel_id: undefined,
    conversation_id: m.conversation_id,
    sender_id: m.sender_id,
    sender_username: m.sender_username,
    ciphertext: new Uint8Array(),
    nonce: new Uint8Array(),
    content_decrypted: parsed?.text,
    reply_to_message_id: m.reply_to_id,
    thread_id: m.thread_id,
    is_pinned: false,
    created_at: new Date(m.sent_at).getTime(),
    attachments: parsed?.attachments ?? [],
    edited_at: m.edited_at,
    deleted_at: m.deleted_at,
    undecryptable: m.undecryptable,
    // A pending edit reads as an ordinary edit; only a failed one is labelled.
    edit_unsent: m.edit_state === 'failed' || m.edit_state === 'rejected',
    ...outboxFields(m.id, m.send_state),
  };
}

type MessagesQueryResult = {
  messages: Message[];
  nextCursor: { sent_at: string; id: string } | null;
};

export function useMessages(channelId: string | null, conversationId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);
  const queryClient = useQueryClient();
  const isChannel = !!channelId;
  const queryKey = isChannel
    ? messageQueryKeys.channel(channelId)
    : messageQueryKeys.conversation(conversationId);
  const targetId = channelId ?? conversationId;

  const query = useQuery({
    queryKey,
    queryFn: async (): Promise<MessagesQueryResult> => {
      // Stubs for sends the store doesn't hold survive the refetch (#1335).
      const cachedStubs = queryClient.getQueryData<MessagesQueryResult>(queryKey)?.messages;
      if (isChannel && channelId) {
        const page = await invoke<MessagePage>('read_channel_messages', {
          channelId,
          limit: 50,
        });
        return {
          messages: carryStubs((page.messages || []).map(transformChannelMessage), cachedStubs),
          nextCursor: page.next_cursor ?? null,
        };
      }

      if (conversationId) {
        const page = await invoke<MessagePage>('read_dm_messages', {
          dmChannelId: conversationId,
          limit: 50,
        });
        return {
          messages: carryStubs((page.messages || []).map(transformChannelMessage), cachedStubs),
          nextCursor: page.next_cursor ?? null,
        };
      }

      return { messages: [], nextCursor: null };
    },
    enabled: !!(channelId || conversationId) && !!currentUser,
    staleTime: 1000 * 30,
  });

  // Background ingest after the local read, skipped if one just ran.
  const ingestInflight = useRef<string | null>(null);
  useEffect(() => {
    if (!currentUser || !targetId) {
      return;
    }
    if (ingestInflight.current === targetId) {
      return;
    }
    if (!shouldIngest(targetId)) {
      return;
    }
    ingestInflight.current = targetId;
    markIngested(targetId);
    const command = isChannel ? 'ingest_channel_envelopes' : 'ingest_dm_envelopes';
    const args = isChannel
      ? { userId: currentUser.id, channelId: targetId }
      : { userId: currentUser.id, dmChannelId: targetId };
    invoke(command, args)
      .catch((e) => {
        console.warn(`[useMessages] ${command} failed:`, e);
      })
      .finally(() => {
        if (ingestInflight.current === targetId) {
          ingestInflight.current = null;
        }
        queryClient.invalidateQueries({ queryKey });
        // A channel ingest stores its siblings' messages too; their badges come from Rust (#1628).
        queryClient.invalidateQueries({ queryKey: unreadQueryKeys.all });
      });
  }, [targetId, isChannel, currentUser?.id]);

  return {
    messages: query.data?.messages ?? [],
    nextCursor: query.data?.nextCursor ?? null,
    isLoading: query.isLoading,
  };
}

/**
 * @deprecated Use useMessages(channelId, null) instead
 */
export function useChannelMessages(channelId: string | null) {
  return useMessages(channelId, null);
}

/**
 * @deprecated Use useMessages(null, conversationId) instead
 */
export function useConversationMessages(conversationId: string | null) {
  return useMessages(null, conversationId);
}

/** Sends leave one conversation one at a time, in tap order (#1636). */
const sendLine = createSendLine();

/**
 * Take the next send turn for a conversation (#1636). Call it synchronously as the user sends,
 * before any await, and hand the turn to `useSendMessage`; a send that never reaches the
 * mutation must release it itself.
 */
export function takeSendTurn(conversationId: string): SendTurn {
  return sendLine.take(conversationId);
}

/** Never blocks the composer: each send waits its `turn`, so they leave in tap order (#1636). */
export function useSendMessage() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({
      channelId,
      conversationId,
      content,
      replyToMessageId,
      threadId,
      messageId,
      attachments,
      turn,
    }: {
      // From takeSendTurn; released in onSettled, which runs on every outcome.
      turn?: SendTurn;
      channelId: string;
      conversationId: string;
      content: string;
      replyToMessageId?: string;
      // Thread root (#825), carried inside the ciphertext only.
      threadId?: string;
      optimisticId?: string; // used by onSuccess to replace the optimistic stub
      // The core's id (#1335), so stub and stored row are one message.
      messageId?: string;
      // Attachments for the outbox to upload (#1335); `content` is the caption.
      attachments?: OutgoingAttachment[];
    }) => {
      if (turn) {
        await turn.ready;
      }
      if (!currentUser) {
        throw new Error("No current user");
      }

      const targetId = channelId || conversationId;
      const id = messageId ?? (await invoke<string>('new_message_id'));
      return await invoke<RawMessage>('send_message', {
        conversationId: targetId,
        senderId: currentUser.id,
        content,
        replyToId: replyToMessageId ?? null,
        threadId: threadId ?? null,
        senderUsername: currentUser.username ?? null,
        messageId: id,
        attachments: attachments ?? [],
      });
    },
    onSuccess: (newMessage, variables) => {
      const queryKey = variables.channelId
        ? messageQueryKeys.channel(variables.channelId)
        : messageQueryKeys.conversation(variables.conversationId);

      const confirmedMessage: Message = {
        ...transformMessage(newMessage),
        sender_username: currentUser?.username ?? undefined,
      };
      // A refetch may already hold the stored row (#1335).
      queryClient.setQueryData<MessagesQueryResult>(queryKey, (old) => {
        const prev = old ?? { messages: [], nextCursor: null };
        return {
          ...prev,
          messages: settleSend(prev.messages, variables.optimisticId, confirmedMessage),
        };
      });

      // Patch the batched previews (#874) containing this conversation.
      const previewId = variables.channelId || variables.conversationId;
      queryClient.setQueriesData<LastMessageMap>(
        {
          queryKey: lastMessageQueryKeys.all,
          // Match on the requested ids, so a first-ever message still lands.
          predicate: (query) => {
            const idList = query.queryKey[2];
            return typeof idList === "string" && idList.split(",").includes(previewId);
          },
        },
        (old) => ({ ...(old ?? {}), [previewId]: confirmedMessage }),
      );

      queryClient.invalidateQueries({ queryKey });

      // Threaded sends (#825) also change the thread and its reply counts.
      if (variables.threadId) {
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.thread(variables.threadId),
        });
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.threadSummaries(
            variables.channelId || variables.conversationId,
          ),
        });
      }
    },
    // The core kept it as failed (#1335): read back so the row shows Retry.
    onError: (_err, variables) => {
      const queryKey = variables.channelId
        ? messageQueryKeys.channel(variables.channelId)
        : messageQueryKeys.conversation(variables.conversationId);
      queryClient.invalidateQueries({ queryKey });
      if (variables.threadId) {
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.thread(variables.threadId),
        });
      }
    },
    // Delivered or failed, the next send in this conversation may go.
    onSettled: (_data, _err, variables) => {
      variables.turn?.release();
    },
  });
}

/** Apply `fn` to every cached conversation page and open thread. */
function updateMessageLists(
  queryClient: ReturnType<typeof useQueryClient>,
  fn: (messages: Message[]) => Message[],
) {
  queryClient.setQueriesData<unknown>({ queryKey: messageQueryKeys.all }, (old: unknown) => {
    if (Array.isArray(old)) {
      return fn(old as Message[]);
    }
    if (old && typeof old === "object" && Array.isArray((old as MessagesQueryResult).messages)) {
      const page = old as MessagesQueryResult;
      return { ...page, messages: fn(page.messages) };
    }
    return old;
  });
}

/** Retry one unsent message (#1335) under the same id, so it never arrives twice. */
export function useRetryMessage() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (outboxId: string) =>
      invoke<RawMessage>("retry_message", { messageId: outboxId }),
    onMutate: (outboxId) => {
      updateMessageLists(queryClient, (msgs) =>
        mapOutboxRows(msgs, outboxId, (m) => ({ ...m, status: "sending" as const })),
      );
    },
    onError: (_err, outboxId) => {
      updateMessageLists(queryClient, (msgs) =>
        mapOutboxRows(msgs, outboxId, (m) => ({ ...m, status: "failed" as const })),
      );
    },
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: messageQueryKeys.all });
      queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
    },
  });
}

/** Discard one unsent message (#1335); the core withdraws a possibly-sent copy first, or refuses. */
export function useDiscardUnsent() {
  const queryClient = useQueryClient();
  return useMutation({
    mutationFn: async (outboxId: string) =>
      invoke<boolean>("discard_unsent_message", { messageId: outboxId }),
    onMutate: (outboxId) => {
      updateMessageLists(queryClient, (msgs) => withoutOutboxRows(msgs, outboxId));
    },
    onSettled: () => {
      queryClient.invalidateQueries({ queryKey: messageQueryKeys.all });
      queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
    },
  });
}

/** Deliver the outbox (#1335) on catch-up, network return or reconnect; never on a timer. */
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
  } catch (err) {
    console.warn("[outbox] flush_outbox failed:", err);
  }
}

export function useDMConversations() {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null),
    queryFn: async (): Promise<DMConversation[]> => {
      if (!currentUser) {
        return [];
      }
      const channels = await invoke<RawDmChannel[]>('list_dm_channels', { userId: currentUser.id });
      return (channels || []).map((c) => {
        const other = c.members.find((m) => m.user_id !== currentUser.id);
        return {
          id: c.id,
          user1_id: currentUser.id,
          user2_identifier: other?.username || other?.user_id || 'Unknown',
          user2_id: other?.user_id,
          user2_avatar_url: other?.avatar_url,
          member_count: c.members.length,
          peers: c.members
            .filter((m) => m.user_id !== currentUser.id)
            .map((m) => ({
              user_id: m.user_id,
              username: m.username,
              avatar_url: m.avatar_url,
            })),
          created_at: new Date(c.created_at).getTime(),
          updated_at: new Date(c.created_at).getTime(),
        };
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
  });
}

/** Newest message per conversation, keyed by conversation id. */
export type LastMessageMap = Record<string, Message>;

/** The newest message per conversation in one IPC call (#874); empty conversations are absent. */
export function useLastMessages(conversationIds: string[]) {
  const currentUser = useObserver(() => appStore.currentUser);

  // Sorted + deduped so the key is a property of the set.
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
      const rows = await invoke<RawChannelMessage[]>('read_last_messages', {
        conversationIds: ids,
      });
      const out: LastMessageMap = {};
      for (const row of rows || []) {
        out[row.conversation_id] = transformChannelMessage(row);
      }
      return out;
    },
    enabled: ids.length > 0 && !!currentUser,
    staleTime: 1000 * 30,
  });
}

export function useLeaveDM() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async (conversationId: string): Promise<void> => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('leave_dm_channel', {
        dmChannelId: conversationId,
        userId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null),
      });
    },
  });
}

export function useCreateOrGetDMConversation() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async (identifier: string): Promise<{ id: string }> => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      const found = await invoke<{ id: string; username?: string } | null>(
        'search_user_by_username',
        { username: identifier },
      );
      if (!found) {
        throw new Error(`User "${identifier}" not found`);
      }
      const channel = await invoke<RawDmChannel>('create_dm_channel', {
        creatorId: currentUser.id,
        memberIds: [found.id],
      });
      return { id: channel.id };
    },
    onSuccess: () => {
      queryClient.invalidateQueries({
        queryKey: messageQueryKeys.dmConversations(currentUser?.id ?? null),
      });
    },
  });
}

export function useDeleteMessage() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async ({ messageId }: { messageId: string }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('delete_message', {
        messageId,
        userId: currentUser.id,
      });
    },
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: messageQueryKeys.all });
      queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
    },
  });
}

type EditMessageVars = {
  conversationId: string;
  channelId?: string;
  messageId: string;
  newContent: string;
};

type EditMessageContext = {
  queryKey: readonly unknown[];
  previousData: MessagesQueryResult | undefined;
};

export function useEditMessage() {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation<void, Error, EditMessageVars, EditMessageContext>({
    mutationFn: async ({ conversationId, messageId, newContent }) => {
      if (!currentUser) {
        throw new Error('No current user');
      }
      await invoke('edit_message', {
        conversationId,
        messageId,
        userId: currentUser.id,
        newContent,
      });
    },
    onMutate: async ({ channelId, conversationId, messageId, newContent }) => {
      const queryKey = channelId
        ? messageQueryKeys.channel(channelId)
        : messageQueryKeys.conversation(conversationId);

      await queryClient.cancelQueries({ queryKey });
      const previousData = queryClient.getQueryData<MessagesQueryResult>(queryKey);

      queryClient.setQueryData<MessagesQueryResult>(queryKey, (old) => {
        if (!old) {
          return old;
        }
        return {
          ...old,
          messages: old.messages.map((m) =>
            m.id === messageId
              ? { ...m, content_decrypted: newContent, edited_at: new Date().toISOString() }
              : m
          ),
        };
      });

      return { queryKey, previousData };
    },
    onError: (_err, _vars, context) => {
      if (context?.previousData !== undefined) {
        queryClient.setQueryData(context.queryKey, context.previousData);
      }
    },
    onSettled: (_data, _err, _vars, context) => {
      if (context) {
        queryClient.invalidateQueries({ queryKey: context.queryKey });
      }
    },
  });
}

/** Mirrors the Rust `ThreadSummary` in `pollis-core/src/commands/messages/types.rs`. */
export type ThreadSummary = {
  thread_id: string;
  reply_count: number;
  last_reply_at?: string;
};

/** Replies in one thread, oldest-first (#825); local-only since `thread_id` is inside the ciphertext. */
export function useThreadMessages(threadId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: messageQueryKeys.thread(threadId),
    queryFn: async (): Promise<Message[]> => {
      if (!threadId) {
        return [];
      }
      const rows = await invoke<RawChannelMessage[]>("read_thread_messages", {
        threadId,
      });
      return (rows || []).map(transformChannelMessage);
    },
    enabled: !!threadId && !!currentUser,
    staleTime: 1000 * 30,
  });
}

/** Reply counts per thread root for a conversation. */
export function useThreadSummaries(conversationId: string | null) {
  const currentUser = useObserver(() => appStore.currentUser);

  return useQuery({
    queryKey: messageQueryKeys.threadSummaries(conversationId),
    queryFn: async (): Promise<Map<string, ThreadSummary>> => {
      if (!conversationId) {
        return new Map();
      }
      const rows = await invoke<ThreadSummary[]>("list_thread_summaries", {
        conversationId,
      });
      return new Map((rows || []).map((r) => [r.thread_id, r]));
    },
    enabled: !!conversationId && !!currentUser,
    staleTime: 1000 * 30,
  });
}
