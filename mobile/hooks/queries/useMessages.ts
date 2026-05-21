// Message read + send + ingest hooks. Mirrors the read paths of
// `frontend/src/hooks/queries/useMessages.ts` — `get_channel_messages` and
// `get_dm_messages` both invoke envelope-ingest internally before reading
// the local DB, so a single call gives a fresh page. Pagination (load
// older history) and infinite-scroll come in a follow-on; this hook
// returns the most-recent `limit` messages newest-first and exposes a
// `useSendMessage` mutation for the composer.

import { useCallback } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { invoke } from "../../lib/native";
import { useAppStore } from "../../stores/appStore";

export type ConversationKind = "channel" | "dm";

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
  reply_to_id?: string | null;
  created_at: number;
  edited_at?: number;
  deleted_at?: number;
  /** Local-only optimistic stub flag. Replaced when `send_message` resolves. */
  pending?: boolean;
}

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
  const currentUser = useAppStore((s) => s.currentUser);
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
  const currentUser = useAppStore((s) => s.currentUser);

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
  const currentUser = useAppStore((s) => s.currentUser);
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
  const currentUser = useAppStore((s) => s.currentUser);
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
  });
}

export function useDeleteMessage(
  conversationId: string | null,
  kind: ConversationKind | null,
) {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((s) => s.currentUser);
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
  });
}

/**
 * Imperative ingest trigger — fire-and-forget. Called from `useFocusEffect`
 * in the chat screen when the screen mounts or refocuses, so the user sees
 * any messages delivered while the app was backgrounded. The refetch
 * interval in `useMessages` covers the steady-state polling.
 */
export function useIngestConversation() {
  const currentUser = useAppStore((s) => s.currentUser);
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
      } catch (e) {
        // Best-effort — ingest is advisory. The next refetch will retry.
        console.warn("[useIngestConversation] ingest failed:", e);
      }
    },
    [currentUser, queryClient],
  );
}
