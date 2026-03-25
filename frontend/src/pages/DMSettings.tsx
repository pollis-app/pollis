import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { useLeaveDM } from "../hooks/queries/useMessages";
import { Button } from "../components/ui/Button";

export const DMSettingsPage: React.FC = () => {
  const navigate = useNavigate();
  const { conversationId } = useParams({ from: "/dms/$conversationId/settings" });
  const { setSelectedConversationId } = useAppStore();
  const leaveDMMutation = useLeaveDM();

  return (
    <div className="flex flex-col h-full">
      <div
        className="flex items-center px-4 py-2 flex-shrink-0 text-xs font-mono"
        style={{
          borderBottom: "1px solid var(--c-border)",
          color: "var(--c-text-muted)",
        }}
      >
        <button
          onClick={() => navigate({ to: "/dms/$conversationId", params: { conversationId } })}
          className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
          style={{ color: "var(--c-text-muted)" }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <ArrowLeft size={12} />
        </button>
        <span style={{ flex: 1, color: "var(--c-text)" }}>Conversation Settings</span>
      </div>
      <div className="flex-1 flex flex-col items-center justify-center gap-4 px-6">
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
    </div>
  );
};
