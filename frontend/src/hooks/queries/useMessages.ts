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

function transformMessage(m: RawMessage): Message {
  return {
    id: m.id,
    channel_id: undefined,
    conversation_id: m.conversation_id,
    sender_id: m.sender_id,
    ciphertext: new Uint8Array(),
    nonce: new Uint8Array(),
    content_decrypted: m.content || '',
    reply_to_message_id: m.reply_to_id,
    is_pinned: false,
    created_at: new Date(m.sent_at).getTime(),
    delivered: true,
    status: 'sent' as const,
    attachments: [],
  };
}

function transformChannelMessage(m: RawChannelMessage): Message {
  return {
    id: m.id,
    channel_id: undefined,
    conversation_id: m.conversation_id,
    sender_id: m.sender_id,
    sender_username: m.sender_username,
    ciphertext: new Uint8Array(),
    nonce: new Uint8Array(),
    content_decrypted: m.content || '',
    reply_to_message_id: m.reply_to_id,
    is_pinned: false,
    created_at: new Date(m.sent_at).getTime(),
    delivered: true,
    status: 'sent' as const,
    attachments: [],
  };
}

export function useMessages(channelId: string | null, conversationId: string | null) {
  const currentUser = useAppStore((state) => state.currentUser);
  const isChannel = !!channelId;
  const queryKey = isChannel
    ? messageQueryKeys.channel(channelId)
    : messageQueryKeys.conversation(conversationId);

  return useQuery({
    queryKey,
    queryFn: async (): Promise<Message[]> => {
      if (isChannel && channelId && currentUser) {
        // Advance the local MLS epoch before decrypting so any pending
        // member-add or member-remove commits are applied first.
        await invoke('process_pending_commits', { conversationId: channelId }).catch(() => {});

        const page = await invoke<MessagePage>('get_channel_messages', {
          userId: currentUser.id,
          channelId,
          limit: 50,
        });
        return (page.messages || []).map(transformChannelMessage);
      }

      if (conversationId && currentUser) {
        await invoke('process_pending_commits', { conversationId }).catch(() => {});

        const page = await invoke<MessagePage>('get_dm_messages', {
          userId: currentUser.id,
          dmChannelId: conversationId,
          limit: 50,
        });
        return (page.messages || []).map(transformChannelMessage);
      }

      return [];
    },
    enabled: !!(channelId || conversationId) && !!currentUser,
    staleTime: 1000 * 30,
    refetchOnWindowFocus: true,
  });
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
      });
    },
    onSuccess: (newMessage, variables) => {
      const queryKey = variables.channelId
        ? messageQueryKeys.channel(variables.channelId)
        : messageQueryKeys.conversation(variables.conversationId);

      // Write the new message into the cache immediately so it appears without
      // waiting for the full refetch round-trip.
      queryClient.setQueryData<Message[]>(queryKey, (old) => {
        const transformed = transformMessage(newMessage);
        return old ? [...old, transformed] : [transformed];
      });

      // Then invalidate in the background so we stay in sync with the server.
      queryClient.invalidateQueries({ queryKey });

      // Notify other participants via the Rust LiveKit connection.
      invoke('publish_ping', {
        roomId: variables.channelId || variables.conversationId,
        channelId: variables.channelId || null,
        conversationId: variables.conversationId || null,
        senderId: currentUser?.id,
        senderUsername: currentUser?.username ?? null,
      }).catch((err) => {
        console.error('[realtime] publish_ping failed:', err);
      });
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
