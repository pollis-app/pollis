import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { useAppStore } from "../stores/appStore";
import { useLeaveDM } from "../hooks/queries/useMessages";
import { Button } from "../components/ui/Button";
import { PageShell } from "../components/Layout/PageShell";

export const DMSettingsPage: React.FC = () => {
  const navigate = useNavigate();
  const { conversationId } = useParams({ from: "/dms/$conversationId/settings" });
  const { setSelectedConversationId } = useAppStore();
  const leaveDMMutation = useLeaveDM();

  return (
    <PageShell
      title="Conversation Settings"
      onBack={() => navigate({ to: "/dms/$conversationId", params: { conversationId } })}
    >
      <div className="h-full flex flex-col items-center justify-center gap-4 px-6">
        {leaveDMMutation.isError && (
          <p className="text-xs font-mono" style={{ color: "#ff6b6b" }}>
            {leaveDMMutation.error instanceof Error ? leaveDMMutation.error.message : "Failed to leave conversation"}
          </p>
        )}
        <Button
          data-testid="dm-settings-leave-button"
          onClick={async () => {
            try {
              await leaveDMMutation.mutateAsync(conversationId);
              setSelectedConversationId(null);
              navigate({ to: "/" });
            } catch {
              // error shown via isError above
            }
          }}
          disabled={leaveDMMutation.isPending}
          isLoading={leaveDMMutation.isPending}
          loadingText="Leaving…"
          variant="danger"
          className="w-full max-w-[280px]"
        >
          Leave conversation
        </Button>
        <Button
          data-testid="dm-settings-cancel-button"
          variant="secondary"
          onClick={() => navigate({ to: "/dms/$conversationId", params: { conversationId } })}
          className="w-full max-w-[280px]"
        >
          Cancel (Esc)
        </Button>
      </div>
    </PageShell>
  );
};
