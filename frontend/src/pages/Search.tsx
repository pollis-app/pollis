import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { SearchView } from "../components/Search/SearchView";
import { useAppStore } from "../stores/appStore";

export const SearchPage: React.FC = () => {
  const navigate = useNavigate();
  const { setSelectedConversationId } = useAppStore();

  return (
    <PageShell title="Search" onBack={() => navigate({ to: "/" })}>
      <SearchView
        onNavigateToConversation={(conversationId) => {
          setSelectedConversationId(conversationId);
          navigate({ to: "/dms/$conversationId", params: { conversationId } });
        }}
      />
    </PageShell>
  );
};
