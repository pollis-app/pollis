import { useQuery, useMutation, useQueryClient } from "@tanstack/react-query";
import { invoke } from "@tauri-apps/api/core";
import { useAppStore } from "../../stores/appStore";
import type { Message, DMConversation } from "../../types";

export const messageQueryKeys = {
  all: ["messages"] as const,
  channel: (channelId: string | null) => ["messages", "channel", channelId] as const,
  conversation: (conversationId: string | null) => ["messages", "conversation", conversationId] as const,
  dmConversations: (userId: string | null) => ["dm-conversations", userId] as const,
};

type RawMessage = {
  id: string;
  conversation_id: string;
  sender_id: string;
  content?: string;
  reply_to_id?: string;
  sent_at: string;
};

// Returned by get_channel_messages — fetches from Turso, decrypts, includes sender_username
type RawChannelMessage = {
  id: string;
  conversation_id: string;
  sender_id: string;
  sender_username?: string;
  ciphertext: string;
  content?: string;
  reply_to_id?: string;
  sent_at: string;
  edited_at?: string;
  deleted_at?: string;
};

type MessagePage = {
  messages: RawChannelMessage[];
  next_cursor: { sent_at: string; id: string } | null;
};

type RawDmChannel = {
  id: string;
  created_by: string;
  created_at: string;
  members: Array<{ user_id: string; username?: string; added_by: string; added_at: string }>;
};

// Parses structured attachment JSON embedded in message content.
// Plain-text messages are returned as-is. Content with attachments looks like:
//   {"_att":[{"key":"media/…","url":"…","name":"…","ct":"…","size":N,"bh":"…","w":N,"h":N}],"_txt":"caption"}
function parseContent(raw: string | undefined): { text: string; attachments: Message['attachments'] } {
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
      attachments: (parsed._att as any[]).map((a) => ({
        id: a.key as string,
        object_key: a.key as string,
        content_hash: a.hash as string,
        filename: a.name as string,
        content_type: a.ct as string,
        file_size: a.size as number,
        uploaded_at: Date.now(),
        blurhash: a.bh as string | undefined,
        width: a.w as number | undefined,
        height: a.h as number | undefined,
      })),
    };
  } catch {
    return { text: raw, attachments: [] };
  }
}

function transformMessage(m: RawMessage): Message {
  // m.content is undefined when the server returned null (e.g. decryption failure).
  // Keep content_decrypted as undefined in that case so MessageList can show
  // [encrypted] instead of silently filtering the message out.
  const parsed = m.content !== undefined ? parseContent(m.content) : null;
  return {
    id: m.id,
    channel_id: undefined,
    conversation_id: m.conversation_id,
    sender_id: m.sender_id,
    ciphertext: new Uint8Array(),
    nonce: new Uint8Array(),
    content_decrypted: parsed?.text,
    reply_to_message_id: m.reply_to_id,
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
  const parsed = m.content !== undefined ? parseContent(m.content) : null;
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
    is_pinned: false,
    created_at: new Date(m.sent_at).getTime(),
    delivered: true,
    status: 'sent' as const,
    attachments: parsed?.attachments ?? [],
    edited_at: m.edited_at,
    deleted_at: m.deleted_at,
  };
}

type MessagesQueryResult = {
  messages: Message[];
  nextCursor: { sent_at: string; id: string } | null;
};

export function useMessages(channelId: string | null, conversationId: string | null) {
  const currentUser = useAppStore((state) => state.currentUser);
  const isChannel = !!channelId;
  const queryKey = isChannel
    ? messageQueryKeys.channel(channelId)
    : messageQueryKeys.conversation(conversationId);

  const query = useQuery({
    queryKey,
    queryFn: async (): Promise<MessagesQueryResult> => {
      if (isChannel && channelId && currentUser) {
        // Advance the local MLS epoch before decrypting so any pending
        // member-add or member-remove commits are applied first.
        await invoke('process_pending_commits', { conversationId: channelId, userId: currentUser.id }).catch(() => {});

        const page = await invoke<MessagePage>('get_channel_messages', {
          userId: currentUser.id,
          channelId,
          limit: 50,
        });
        return {
          messages: (page.messages || []).map(transformChannelMessage),
          nextCursor: page.next_cursor ?? null,
        };
      }

      if (conversationId && currentUser) {
        // Drain any pending MLS Welcome messages first — the DM creator may have
        // added us to the MLS group while we were already online.
        await invoke('poll_mls_welcomes', { userId: currentUser.id }).catch(() => {});
        await invoke('process_pending_commits', { conversationId, userId: currentUser.id }).catch(() => {});

        const page = await invoke<MessagePage>('get_dm_messages', {
          userId: currentUser.id,
          dmChannelId: conversationId,
          limit: 50,
        });
        return {
          messages: (page.messages || []).map(transformChannelMessage),
          nextCursor: page.next_cursor ?? null,
        };
      }

      return { messages: [], nextCursor: null };
    },
    enabled: !!(channelId || conversationId) && !!currentUser,
    staleTime: 1000 * 30,
    refetchOnWindowFocus: true,
  });

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
  const currentUser = useAppStore((state) => state.currentUser);

  return useMutation({
    mutationFn: async ({
      channelId,
      conversationId,
      content,
      replyToMessageId,
    }: {
      channelId: string;
      conversationId: string;
      content: string;
      replyToMessageId?: string;
      optimisticId?: string; // used by onSuccess to replace the optimistic stub
    }) => {
      if (!currentUser) {
        throw new Error("No current user");
      }

      const targetId = channelId || conversationId;
      return await invoke<RawMessage>('send_message', {
        conversationId: targetId,
        senderId: currentUser.id,
        content,
        replyToId: replyToMessageId ?? null,
        senderUsername: currentUser.username ?? null,
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
      queryClient.setQueryData<MessagesQueryResult>(queryKey, (old) => {
        const prev = old ?? { messages: [], nextCursor: null };
        const filtered = variables.optimisticId
          ? prev.messages.filter((m) => m.id !== variables.optimisticId)
          : prev.messages;
        return { ...prev, messages: [...filtered, confirmedMessage] };
      });

      // Update the last-message preview immediately.
      const lastMsgKey = variables.channelId
        ? ["last-message", "channel", variables.channelId]
        : ["last-message", "conversation", variables.conversationId];
      queryClient.setQueryData(lastMsgKey, confirmedMessage);

      // Then invalidate in the background so we stay in sync with the server.
      queryClient.invalidateQueries({ queryKey });
    },
  });
}

export function useDMConversations() {
  const currentUser = useAppStore((state) => state.currentUser);

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
          created_at: new Date(c.created_at).getTime(),
          updated_at: new Date(c.created_at).getTime(),
        };
      });
    },
    enabled: !!currentUser,
    staleTime: 1000 * 60,
    refetchOnWindowFocus: true,
  });
}

export function useLastMessage(channelId: string | null, conversationId: string | null) {
  const currentUser = useAppStore((state) => state.currentUser);
  const isChannel = !!channelId;
  const queryKey = isChannel
    ? (["last-message", "channel", channelId] as const)
    : (["last-message", "conversation", conversationId] as const);

  return useQuery({
    queryKey,
    queryFn: async (): Promise<Message | null> => {
      if (isChannel && channelId && currentUser) {
        const page = await invoke<MessagePage>('get_channel_messages', {
          userId: currentUser.id,
          channelId,
          limit: 1,
        });
        const msgs = (page.messages || []).map(transformChannelMessage);
        return msgs[msgs.length - 1] ?? null;
      }
      if (conversationId && currentUser) {
        const page = await invoke<MessagePage>('get_dm_messages', {
          userId: currentUser.id,
          dmChannelId: conversationId,
          limit: 1,
        });
        const msgs = (page.messages || []).map(transformChannelMessage);
        return msgs[msgs.length - 1] ?? null;
      }
      return null;
    },
    enabled: !!(channelId || conversationId) && !!currentUser,
    staleTime: 1000 * 30,
    refetchOnWindowFocus: true,
  });
}

export function useLeaveDM() {
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);

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
  const currentUser = useAppStore((state) => state.currentUser);

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
  const currentUser = useAppStore((state) => state.currentUser);

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
      queryClient.invalidateQueries({ queryKey: ['last-message'] });
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
  const currentUser = useAppStore((state) => state.currentUser);

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
