// Message read + send + ingest hooks. Mirrors the read paths of
// `frontend/src/hooks/queries/useMessages.ts` — `get_channel_messages` and
// `get_dm_messages` both invoke envelope-ingest internally before reading
// the local DB, so a single call gives a fresh page. Pagination (load
// older history) and infinite-scroll come in a follow-on; this hook
// returns the most-recent `limit` messages newest-first and exposes a
// `useSendMessage` mutation for the composer.

import { useCallback, useMemo } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { appStore } from "../../stores/appStore";
import { useObserver } from "mobx-react-lite";
import type { MessageAttachment } from "../../types";

export type ConversationKind = "channel" | "dm";

// Wire shape of a single attachment inside the `_att` array embedded in
// message content JSON — the DECODE half of desktop's
// `utils/attachmentEnvelope.ts` (see `parseContent`).
interface AttachmentWire {
  key: string;
  hash: string;
  name: string;
  ct: string;
  size: number;
  bh?: string;
  w?: number;
  h?: number;
}

export interface RawChannelMessage {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  ciphertext?: string;
  content?: string;
  reply_to_id?: string | null;
  sent_at: string;
  edited_at?: string;
  deleted_at?: string;
}

export interface MessagePage {
  messages: RawChannelMessage[];
  next_cursor: { sent_at: string; id: string } | null;
}

export interface Message {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  content: string;
  attachments?: MessageAttachment[];
  reply_to_id?: string | null;
  created_at: number;
  edited_at?: number;
  deleted_at?: number;
  /** Local-only optimistic stub flag. Replaced when `send_message` resolves. */
  pending?: boolean;
}

// Parses structured attachment JSON embedded in message content — the same
// `{"_att":[…],"_txt":"caption"}` envelope desktop's useMessages decodes.
// Plain-text messages are returned as-is with no attachments.
function parseContent(raw: string | undefined): {
  text: string;
  attachments: MessageAttachment[];
} {
  if (!raw?.startsWith("{")) {
    return { text: raw ?? "", attachments: [] };
  }
  try {
    const parsed = JSON.parse(raw);
    if (!Array.isArray(parsed._att)) {
      return {
        text: typeof parsed?._txt === "string" ? parsed._txt : raw,
        attachments: [],
      };
    }
    return {
      text: typeof parsed._txt === "string" ? parsed._txt : "",
      attachments: (parsed._att as AttachmentWire[]).map((a) => ({
        id: a.key,
        object_key: a.key,
        content_hash: a.hash,
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
    // Fall through — treat as plain text.
    return { text: raw, attachments: [] };
  }
}

function transform(raw: RawChannelMessage): Message {
  const { text, attachments } = parseContent(raw.content);
  return {
    id: raw.id,
    conversation_id: raw.conversation_id,
    sender_id: raw.sender_id,
    sender_username: raw.sender_username,
    content: text,
    attachments: attachments.length > 0 ? attachments : undefined,
    reply_to_id: raw.reply_to_id ?? null,
    created_at: new Date(raw.sent_at).getTime(),
    edited_at: raw.edited_at ? new Date(raw.edited_at).getTime() : undefined,
    deleted_at: raw.deleted_at ? new Date(raw.deleted_at).getTime() : undefined,
  };
}

export const messageQueryKeys = {
  all: ["messages"] as const,
  conversation: (
    conversationId: string | null,
    kind: ConversationKind | null,
  ) => ["messages", kind, conversationId] as const,
};

export const lastMessageQueryKeys = {
  all: ["last-message"] as const,
  batch: (conversationIds: string[]) =>
    ["last-message", "batch", conversationIds.join(",")] as const,
};

/**
 * Fetch the most recent `limit` messages for a channel or DM. `get_*_messages`
 * runs envelope ingest itself before reading, so each call picks up newly
 * delivered messages — no separate ingest hook is required for the basic
 * polling path. The chat screen still triggers an explicit ingest via
 * `useIngestConversation` on focus so a returning user sees fresh content
 * immediately, without waiting for the next refetch.
 */
export function useMessages(
  conversationId: string | null,
  kind: ConversationKind | null,
  opts?: { limit?: number; refetchIntervalMs?: number | false },
) {
  const currentUser = useObserver(() => appStore.currentUser);
  const limit = opts?.limit ?? 50;
  // No polling by default. Realtime push (a follow-on PR) will invalidate
  // this query when a new envelope arrives; until then the focus-effect
  // ingest in the chat screen covers the "open a chat and see what was
  // sent while I was away" case. A periodic poll would just be ripped
  // out the moment realtime lands.
  const refetchInterval = opts?.refetchIntervalMs ?? false;

  return useQuery({
    queryKey: messageQueryKeys.conversation(conversationId, kind),
    queryFn: async (): Promise<{ messages: Message[]; nextCursor: MessagePage["next_cursor"] }> => {
      if (!conversationId || !kind || !currentUser) {
        return { messages: [], nextCursor: null };
      }
      const cmd =
        kind === "channel" ? "get_channel_messages" : "get_dm_messages";
      const args =
        kind === "channel"
          ? { userId: currentUser.id, channelId: conversationId, limit }
          : { userId: currentUser.id, dmChannelId: conversationId, limit };
      const page = await invoke<MessagePage>(cmd, args);
      // Server returns newest-first; reverse for chronological render.
      const transformed = (page.messages ?? []).map(transform).reverse();
      return { messages: transformed, nextCursor: page.next_cursor };
    },
    enabled: !!(conversationId && kind && currentUser),
    staleTime: 1000 * 15,
    refetchInterval,
  });
}

/** Newest message per conversation, keyed by conversation id. */
export type LastMessageMap = Record<string, Message>;

/**
 * The newest message for every conversation in `conversationIds`, in ONE
 * bridge call — mirrors desktop's `useLastMessages` (#874/#936): one batched
 * `read_last_messages` per list, never one call per row, and no focus/interval
 * polling — realtime events invalidate `lastMessageQueryKeys.all` instead.
 *
 * Previews come from the LOCAL store (the message table only holds what this
 * device has ingested and decrypted), so a conversation with no local
 * messages is simply absent from the map — callers treat "no entry" as "no
 * preview" and render the row without one.
 */
export function useLastMessages(conversationIds: string[]) {
  const currentUser = useObserver(() => appStore.currentUser);

  // Sorted + de-duplicated so the cache key is a property of the SET, not of
  // the render order the caller happened to produce.
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

/**
 * One-line preview text for a conversation row. Deleted messages and
 * attachment-only messages get placeholders instead of raw envelope JSON.
 */
export function previewText(message: Message | undefined): string | null {
  if (!message) {
    return null;
  }
  if (message.deleted_at) {
    return "Message deleted";
  }
  if (message.content) {
    return message.content;
  }
  if (message.attachments && message.attachments.length > 0) {
    return "Attachment";
  }
  return null;
}

/**
 * Send a text message. Optimistic: the new message is appended to the
 * cache immediately with `pending: true`, then replaced with the
 * server-confirmed row on success (or removed on failure).
 */
export function useSendMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);

  return useMutation({
    mutationFn: async (vars: { content: string; replyToId?: string }) => {
      if (!conversationId || !currentUser) {
        throw new Error("No active conversation");
      }
      const raw = await invoke<RawChannelMessage>("send_message", {
        conversationId,
        senderId: currentUser.id,
        content: vars.content,
        replyToId: vars.replyToId ?? null,
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
        created_at: Date.now(),
        pending: true,
      };
      const previous = queryClient.getQueryData<{
        messages: Message[];
        nextCursor: MessagePage["next_cursor"];
      }>(key);
      queryClient.setQueryData(key, {
        messages: [...(previous?.messages ?? []), optimistic],
        nextCursor: previous?.nextCursor ?? null,
      });
      return { previous, optimisticId, key };
    },
    onSuccess: (confirmed, _vars, ctx) => {
      // The sent message is now the conversation's newest — refresh the
      // batched list previews.
      queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
      if (!ctx) {
        return;
      }
      queryClient.setQueryData<{
        messages: Message[];
        nextCursor: MessagePage["next_cursor"];
      }>(ctx.key, (cache) => {
        if (!cache) {
          return cache;
        }
        return {
          ...cache,
          messages: cache.messages.map((m) =>
            m.id === ctx.optimisticId ? confirmed : m,
          ),
        };
      });
    },
    onError: (_e, _vars, ctx) => {
      if (!ctx?.previous) {
        return;
      }
      // Roll back the optimistic stub on failure.
      queryClient.setQueryData(ctx.key, ctx.previous);
    },
  });
}

/** React (toggle) helper. Returns a mutation that toggles a single emoji
 *  on a message — checks the current reaction state by sending
 *  add_reaction or remove_reaction. The caller is responsible for
 *  tracking whether they already reacted; this hook just dispatches the
 *  intent. */
export function useToggleReaction(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useObserver(() => appStore.currentUser);
  return useMutation({
    mutationFn: async (vars: {
      messageId: string;
      emoji: string;
      mode: "add" | "remove";
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }
      const cmd = vars.mode === "add" ? "add_reaction" : "remove_reaction";
      await invoke(cmd, {
        messageId: vars.messageId,
        userId: currentUser.id,
        emoji: vars.emoji,
      });
    },
    onSuccess: () => {
      if (conversationId && kind) {
        queryClient.invalidateQueries({
          queryKey: messageQueryKeys.conversation(conversationId, kind),
        });
      }
    },
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
      const previous = queryClient.getQueryData<{
        messages: Message[];
        nextCursor: MessagePage["next_cursor"];
      }>(key);
      queryClient.setQueryData(key, (cache: typeof previous) => {
        if (!cache) {
          return cache;
        }
        return {
          ...cache,
          messages: cache.messages.map((m) =>
            m.id === vars.messageId
              ? { ...m, content: vars.newContent, edited_at: Date.now() }
              : m,
          ),
        };
      });
      return { previous, key };
    },
    onError: (_e, _vars, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(ctx.key, ctx.previous);
      }
    },
    onSettled: () => {
      // The edited message may be a conversation's newest — refresh previews.
      queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
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
      const previous = queryClient.getQueryData<{
        messages: Message[];
        nextCursor: MessagePage["next_cursor"];
      }>(key);
      queryClient.setQueryData(key, (cache: typeof previous) => {
        if (!cache) {
          return cache;
        }
        return {
          ...cache,
          messages: cache.messages.filter((m) => m.id !== messageId),
        };
      });
      return { previous, key };
    },
    onError: (_e, _vars, ctx) => {
      if (ctx?.previous) {
        queryClient.setQueryData(ctx.key, ctx.previous);
      }
    },
    onSettled: () => {
      // The deleted message may have been a conversation's newest — refresh
      // previews so the row doesn't keep showing it.
      queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
    },
  });
}

/**
 * Imperative ingest trigger — fire-and-forget. Called from `useFocusEffect`
 * in the chat screen when the screen mounts or refocuses, so the user sees
 * any messages delivered while the app was backgrounded. The refetch
 * interval in `useMessages` covers the steady-state polling.
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
        // Ingest may have landed a newer message — refresh list previews too.
        queryClient.invalidateQueries({ queryKey: lastMessageQueryKeys.all });
      } catch (e) {
        // Best-effort — ingest is advisory. The next refetch will retry.
        console.warn("[useIngestConversation] ingest failed:", e);
      }
    },
    [currentUser, queryClient],
  );
}
