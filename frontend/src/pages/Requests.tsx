import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { useAppStore } from "../stores/appStore";
import {
  useDMRequests,
  useAcceptDMRequest,
  useBlockUser,
} from "../hooks/queries";
import { useLastMessage } from "../hooks/queries/useMessages";
import { Button } from "../components/ui/Button";
import { NavigableList } from "../components/ui/NavigableList";
import { ScrambleText } from "../components/ui/ScrambleText";
import { timeAgo } from "../utils/timeAgo";
import type { DmChannel } from "../types";

// Renders the first/latest decrypted message body of a DM request. The
// sender's name is shown separately on the row, so no prefix is needed
// here. The MLS envelope is decrypted during the normal polling flow —
// acceptance is a UI-only flag, not a decryption gate.
const RequestPreview: React.FC<{ dmChannelId: string }> = ({ dmChannelId }) => {
  const { data: message, isLoading } = useLastMessage(null, dmChannelId);

  if (isLoading) {
    return <ScrambleText text={null} placeholderLength={28} typeSpeed={25} />;
  }

  const text = message?.content_decrypted ?? "(no message yet)";
  return <ScrambleText text={text} placeholderLength={28} typeSpeed={25} />;
};

export const Requests: React.FC = () => {
  const navigate = useNavigate();
  const currentUser = useAppStore((state) => state.currentUser);
  const { data: requests = [], isLoading } = useDMRequests();
  const acceptMutation = useAcceptDMRequest();
  const blockMutation = useBlockUser();

  const findOther = (c: DmChannel) => {
    return c.members.find((m) => m.user_id !== currentUser?.id);
  };

  const handleAccept = async (channelId: string) => {
    try {
      await acceptMutation.mutateAsync(channelId);
      navigate({ to: "/dms/$conversationId", params: { conversationId: channelId } });
    } catch (err) {
      console.error("Failed to accept DM request:", err);
    }
  };

  const handleBlock = async (c: DmChannel) => {
    const other = findOther(c);
    if (!other) {
      return;
    }
    try {
      await blockMutation.mutateAsync(other.user_id);
    } catch (err) {
      const msg = err instanceof Error ? err.message : String(err);
      console.error("Failed to block user:", msg);
    }
  };

  return (
    <div
      data-testid="requests-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: "var(--c-bg)" }}
    >
      <NavigableList
        items={requests}
        isLoading={isLoading}
        emptyLabel="No pending requests."
        getKey={(c) => c.id}
        rowTestId={(c) => `request-${c.id}`}
        onEnterRow={(c) => handleAccept(c.id)}
        renderRow={(c) => {
          const other = findOther(c);
          const name = other?.username ?? other?.user_id ?? "Unknown";
          const groupSize = c.members.length;
          return (
            <div className="flex-1 min-w-0 flex flex-col gap-0.5">
              <span
                className="text-sm font-mono font-medium truncate"
                style={{ color: "var(--c-text)" }}
              >
                {name}
                {groupSize > 2 && (
                  <span
                    className="ml-2 text-xs"
                    style={{ color: "var(--c-text-muted)" }}
                  >
                    (+{groupSize - 2} others)
                  </span>
                )}
              </span>
              <span
                className="text-xs font-mono truncate"
                style={{ color: "var(--c-text-muted)" }}
              >
                <RequestPreview dmChannelId={c.id} />
              </span>
            </div>
          );
        }}
        controls={(c) => [
          <Button size="sm"
            data-testid={`accept-request-${c.id}`}
            onClick={() => handleAccept(c.id)}
            disabled={acceptMutation.isPending || blockMutation.isPending}
            variant="primary"
          >
            accept
          </Button>,
          <Button size="sm"
            data-testid={`block-request-${c.id}`}
            onClick={() => handleBlock(c)}
            disabled={acceptMutation.isPending || blockMutation.isPending}
            variant="secondary"
          >
            block
          </Button>,
        ]}
        trailing={(c) => <span>{timeAgo(c.created_at)}</span>}
      />
    </div>
  );
};
