import React, { useEffect } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { MainContent } from "../components/Layout/MainContent";
import { useDMConversations } from "../hooks/queries/useMessages";
import { useAppStore } from "../stores/appStore";

export const DMPage: React.FC = () => {
  const navigate = useNavigate();
  const { conversationId } = useParams({ from: "/dms/$conversationId" });
  const setSelectedConversationId = useAppStore((s) => s.setSelectedConversationId);

  useEffect(() => {
    setSelectedConversationId(conversationId);
    return () => { setSelectedConversationId(null); };
  }, [conversationId, setSelectedConversationId]);

  const { data: conversations = [] } = useDMConversations();
  const conv = conversations.find((c) => c.id === conversationId);

  const title = conv ? `@${conv.user2_identifier}` : "Direct Message";

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
          onClick={() => navigate({ to: "/dms" })}
          className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
          style={{ color: "var(--c-text-muted)" }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <ArrowLeft size={12} />
        </button>
        <span>{title}</span>
      </div>
      <div className="flex-1 overflow-hidden flex flex-col min-h-0">
        <MainContent />
      </div>
    </div>
  );
};
