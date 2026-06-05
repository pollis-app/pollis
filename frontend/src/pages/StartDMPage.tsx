import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { StartDM } from "./StartDM";

export const StartDMPage: React.FC = observer(() => {
  const navigate = useNavigate();
  const { setSelectedConversationId } = appStore;

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
});
