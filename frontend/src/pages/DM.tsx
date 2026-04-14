import React, { useEffect } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { Ban } from "lucide-react";
import { MainContent } from "../components/Layout/MainContent";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useBlockUser } from "../hooks/queries";
import { Button } from "../components/ui/Button";
import { useAppStore } from "../stores/appStore";
import { invoke } from "@tauri-apps/api/core";

type RawDmMember = { user_id: string; username?: string };
type RawDmChannel = { id: string; members: RawDmMember[] };

export const DMPage: React.FC = () => {
  const navigate = useNavigate();
  const { conversationId } = useParams({ from: "/dms/$conversationId" });
  const setSelectedConversationId = useAppStore((s) => s.setSelectedConversationId);
  const currentUser = useAppStore((s) => s.currentUser);
  const blockMutation = useBlockUser();

  const [otherUserId, setOtherUserId] = React.useState<string | null>(null);
  const [memberCount, setMemberCount] = React.useState<number>(0);

  useEffect(() => {
    setSelectedConversationId(conversationId);
    return () => { setSelectedConversationId(null); };
  }, [conversationId, setSelectedConversationId]);

  // Fetch member list for the conversation so we can target the right
  // user_id when blocking. list_dm_channels returns all channels for the
  // current user, including members.
  useEffect(() => {
    let cancelled = false;
    if (!currentUser) {
      return;
    }
    invoke<RawDmChannel[]>("list_dm_channels", { userId: currentUser.id })
      .then((channels) => {
        if (cancelled) {
          return;
        }
        const match = channels.find((c) => c.id === conversationId);
        if (!match) {
          return;
        }
        setMemberCount(match.members.length);
        const other = match.members.find((m) => m.user_id !== currentUser.id);
        setOtherUserId(other?.user_id ?? null);
      })
      .catch(() => {});
    return () => { cancelled = true; };
  }, [currentUser, conversationId]);

  const { data: conversations = [] } = useDMConversations();
  const conv = conversations.find((c) => c.id === conversationId);

  const title = conv ? `@${conv.user2_identifier}` : "Direct Message";

  // Blocks are per-user, not per-channel. For group DMs (3+ members) we'd need
  // a submenu to pick which user to block — skipped for now.
  const canBlock = memberCount === 2 && otherUserId != null;

  const handleBlock = async () => {
    if (!otherUserId) {
      return;
    }
    try {
      await blockMutation.mutateAsync(otherUserId);
      navigate({ to: "/dms" });
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("Failed to block user:", msg);
    }
  };

  return (
    <div className="flex flex-col h-full">
      <div
        className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text-muted)",
        }}
      >
        <span style={{ flex: 1 }}>{title}</span>
        {canBlock && (
          <Button
            data-testid="dm-header-block"
            onClick={handleBlock}
            disabled={blockMutation.isPending}
            variant="ghost"
            aria-label="Block user"
            className="!px-2 !py-0.5"
          >
            <Ban size={12} />
            <span>block</span>
          </Button>
        )}
      </div>
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent />
      </div>
    </div>
  );
};
