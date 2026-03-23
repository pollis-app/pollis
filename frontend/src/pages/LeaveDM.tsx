import React from "react";
import { invoke } from "@tauri-apps/api/core";
import { useNavigate, useParams } from "@tanstack/react-router";
import { useQueryClient } from "@tanstack/react-query";
import { useAppStore } from "../stores/appStore";
import { useEffect } from "react";

export const LeaveDM: React.FC = () => {
  const { conversationId } = useParams({ strict: false }) as { conversationId: string };
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const currentUser = useAppStore((state) => state.currentUser);
  const setSelectedConversationId = useAppStore((state) => state.setSelectedConversationId);

  // Esc navigates back
  useEffect(() => {
    const handle = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        navigate({ to: "/c/$conversationId", params: { conversationId } });
      }
    };
    window.addEventListener("keydown", handle);
    return () => window.removeEventListener("keydown", handle);
  }, [conversationId, navigate]);

  const handleLeave = async () => {
    if (!currentUser) {
      return;
    }
    try {
      await invoke("leave_dm_channel", {
        dmChannelId: conversationId,
        userId: currentUser.id,
      });
      await queryClient.invalidateQueries({ queryKey: ["dm_channels"] });
      setSelectedConversationId(null);
      navigate({ to: "/" });
    } catch (err) {
      console.error("[LeaveDM] failed:", err);
    }
  };

  return (
    <div
      data-testid="leave-dm-page"
      className="flex-1 flex flex-col items-center justify-center"
      style={{ background: "var(--c-bg)" }}
    >
      <div
        className="flex flex-col gap-4"
        style={{ width: "100%", maxWidth: 320, padding: "2rem" }}
      >
        <p className="text-sm font-mono" style={{ color: "var(--c-text)" }}>
          Leave this conversation?
        </p>
        <p className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          It will no longer appear in your sidebar. The other person can still
          see the conversation.
        </p>

        <div className="flex flex-col gap-2" style={{ marginTop: "0.5rem" }}>
          <button
            data-testid="leave-dm-confirm-button"
            onClick={handleLeave}
            className="w-full py-2 px-4 font-mono text-xs"
            style={{
              background: "transparent",
              border: "1px solid hsl(0 70% 50% / 40%)",
              borderRadius: "4px",
              color: "hsl(0 70% 65%)",
              cursor: "pointer",
            }}
            onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.background = "hsl(0 70% 50% / 10%)"; }}
            onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.background = "transparent"; }}
          >
            Leave conversation
          </button>
          <button
            data-testid="leave-dm-cancel-button"
            onClick={() => navigate({ to: "/c/$conversationId", params: { conversationId } })}
            className="w-full py-1 font-mono text-xs"
            style={{ color: "var(--c-text-muted)", background: "transparent", border: "none", cursor: "pointer" }}
          >
            Cancel (Esc)
          </button>
        </div>
      </div>
    </div>
  );
};
