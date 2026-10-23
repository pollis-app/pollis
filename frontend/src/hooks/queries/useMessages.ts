import { useEffect, useMemo, useRef } from "react";
import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../bridge";
import { appStore } from "../../stores/appStore";
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

// Per-conversation timestamp of the last background ingest. Used to debounce
// rapid channel-switching so we don't fire one ingest per click. Realtime
// hints also write here when they trigger an ingest, so a focus immediately
// after a realtime-driven ingest skips the redundant call.
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
  // One entry per SET of conversations, not per conversation (#874). The
  // sidebar asks for all its preview rows at once, so the cache key is the
  // request, and every invalidation path targets the `all` prefix.
  batch: (conversationIds: string[]) =>
    ["last-message", "batch", conversationIds.join(",")] as const,
};

// Wire shape of a single attachment inside the `_att` array embedded in
// message content JSON (see parseContent).
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

// Returned by get_channel_messages — fetches from Turso, decrypts, includes sender_username
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
};

/** Mirror of `pollis_core::commands::messages::OutboxReport`. */
type OutboxReport = {
  delivered: string[];
  failed: string[];
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

// Parses structured attachment JSON embedded in message content.
// Plain-text messages are returned as-is. Content with attachments looks like:
//   {"_att":[{"key":"media/…","url":"…","name":"…","ct":"…","size":N,"hash":"…","kv":N,"sha":"…","bh":"…","w":N,"h":N}],"_txt":"caption"}
// Exported for the surfaces that render the SAME content-string shape from
// other sources — pinned-message snapshots (#99) and vault entries (#107) —
// so there is one decoder for the envelope, not three. `origin` says where
// the content lives, which is what names each attachment's key (#1382).
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
      // An entry with no `key` is one the outbox has not uploaded yet (#1335):
      // drawn like an optimistic stub's attachment, pending.
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
      })),
    };
  } catch {
    return { text: raw, attachments: [] };
  }
}

/**
 * The key an attachment reference names, or null when it cannot be opened: no
 * object yet (uploading), or no key version and digest (a reference from
 * before #1382, sealed under the retired scheme).
 */
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
  // m.content is undefined when the server returned null (e.g. decryption failure).
  // Keep content_decrypted as undefined in that case so MessageList can show
  // [encrypted] instead of silently filtering the message out.
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
  // m.content is undefined when the server returned null (e.g. decryption failure).
  // Keep content_decrypted as undefined in that case so MessageList can show
  // [encrypted] instead of silently filtering the message out.
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
    // `delivered` / `status` / `outbox_id` from the durable outbox (#1335).
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
      // A stub for a send the store does not hold (yet, or ever — a failed
      // upload) survives the refetch; one whose message it does hold steps
      // aside for the stored row (#1335).
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

  // Fire ingest in the background after the local read mounts. Skipped if a
  // recent ingest (incl. one driven by a realtime hint) already covered this
  // conversation. Invalidates the query on completion so newly-decrypted
  // messages and freshly-cached usernames show up.
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
    }: {
      channelId: string;
      conversationId: string;
      content: string;
      replyToMessageId?: string;
      // ULID of the thread root when sending into a thread (#825). Carried
      // inside the MLS ciphertext, never in the DS request body.
      threadId?: string;
      optimisticId?: string; // used by onSuccess to replace the optimistic stub
      // The core's id for this message (#1335), from `new_message_id`. The
      // caller that drew a stub passes it so stub and stored row are one
      // message; otherwise one is asked for here.
      messageId?: string;
      // Attachments for the outbox to upload (#1335); `content` is then the
      // caption.
      attachments?: OutgoingAttachment[];
    }) => {
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

      // Replace the optimistic stub (if any) with the confirmed server message,
      // or append it if there was no stub.
      const confirmedMessage: Message = {
        ...transformMessage(newMessage),
        sender_username: currentUser?.username ?? undefined,
      };
      // A refetch may already hold the stored row (#1335): replace it rather
      // than appending a second copy.
      queryClient.setQueryData<MessagesQueryResult>(queryKey, (old) => {
        const prev = old ?? { messages: [], nextCursor: null };
        return {
          ...prev,
          messages: settleSend(prev.messages, variables.optimisticId, confirmedMessage),
        };
      });

      // Update the last-message preview immediately. Previews are batched per
      // set of conversations (#874), so patch the entry inside whichever cached
      // batches contain this conversation rather than writing a per-id key.
      const previewId = variables.channelId || variables.conversationId;
      queryClient.setQueriesData<LastMessageMap>(
        {
          queryKey: lastMessageQueryKeys.all,
          // Only the batches that ASKED for this conversation. Keyed on the id
          // list rather than on the cached data, so the first message ever sent
          // into a conversation still lands — an empty conversation has no
          // entry to match against, but its batch still named it.
          predicate: (query) => {
            const idList = query.queryKey[2];
            return typeof idList === "string" && idList.split(",").includes(previewId);
          },
        },
        (old) => ({ ...(old ?? {}), [previewId]: confirmedMessage }),
      );

      // Then invalidate in the background so we stay in sync with the server.
      queryClient.invalidateQueries({ queryKey });

      // A threaded send (#825) also changes the open thread and that
      // conversation's reply counts; without this the thread panel would show
      // the reply only after a refetch on focus.
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
    // The core kept the message and marked it failed (#1335): read it back so
    // the row shows Retry — in the thread panel too, which draws no stub.
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
  });
}

/**
 * Apply `fn` to every cached message list — conversation pages and open
 * threads, the two shapes under the `messages` prefix that hold rows.
 */
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

/**
 * Retry one unsent message (#1335). `retry_message` re-runs delivery under
 * the same id, so it never arrives twice. The row reads as sending while it
 * runs; the refetch afterwards shows what the core settled on.
 */
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

/**
 * Throw away one unsent message (#1335). If an earlier attempt may have reached
 * the server (it was ever sealed), the core withdraws it from everyone first —
 * and refuses the discard if it cannot, so the row stays rather than leaving a
 * delivered copy the sender cannot delete. Also clears a stub the core never
 * stored.
 */
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

/**
 * Deliver whatever the durable outbox holds (#1335) — run once the launch
 * catch-up has finished, when the browser reports the network back, and on a
 * realtime reconnect. Event-driven, never on a timer. The core already drains
 * after every unlock; this waits for that and refreshes what it changed.
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

/**
 * The newest message for every conversation in `conversationIds`, in ONE IPC
 * call (#874).
 *
 * Previously each preview row owned its own `useLastMessage`, so a sidebar of
 * N channels plus M DMs fired N+M `read_channel_messages` / `read_dm_messages`
 * calls — on mount, again on every window focus, and again on every
 * `deleted_message` event. The backend does the same indexed scan and the same
 * username hydration either way, so asking once is strictly cheaper.
 *
 * Callers pass the ids and hand each row its own entry; the map is keyed by
 * conversation id and a conversation with no messages is simply absent.
 */
export function useLastMessages(conversationIds: string[]) {
  const currentUser = useObserver(() => appStore.currentUser);

  // Sorted + de-duplicated so the cache key is a property of the SET, not of
  // the render order the caller happened to produce. Two renders listing the
  // same conversations must hit the same entry rather than refetching.
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
      // Invalidate all message caches so the deleted message disappears.
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

/**
 * Replies in one thread, oldest-first (#825).
 *
 * Local-only by construction: `thread_id` lives inside the MLS ciphertext, so
 * the DS cannot be asked for a thread. This returns what this device has
 * decrypted — the same bounded-history behaviour channels already have.
 */
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

/**
 * Reply counts per thread root for a conversation, so a channel row can show
 * "N replies" without loading every thread.
 */
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
