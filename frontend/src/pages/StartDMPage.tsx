import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { useAppStore } from "../stores/appStore";
import { StartDM } from "./StartDM";

export const StartDMPage: React.FC = () => {
  const navigate = useNavigate();
  const { setSelectedConversationId } = useAppStore();

  return (
    <PageShell title="New Message">
      <StartDM
        onSuccess={(conversationId) => {
          setSelectedConversationId(conversationId);
          navigate({ to: "/dms/$conversationId", params: { conversationId } });
        }}
      />
    </PageShell>
  );
};
