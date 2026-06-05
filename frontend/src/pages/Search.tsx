import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { SearchView } from "../components/Search/SearchView";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";

export const SearchPage: React.FC = observer(() => {
  const navigate = useNavigate();
  const { setSelectedConversationId } = appStore;

  return (
    <PageShell title="Search">
      <SearchView
        onNavigateToConversation={(conversationId) => {
          setSelectedConversationId(conversationId);
          navigate({ to: "/dms/$conversationId", params: { conversationId } });
        }}
      />
    </PageShell>
  );
});
