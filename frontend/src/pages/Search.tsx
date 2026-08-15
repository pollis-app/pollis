import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { SearchView } from "../components/Search/SearchView";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";

export const SearchPage: React.FC = observer(() => {
  const { t } = useTranslation("search");
  const navigate = useNavigate();
  const { setSelectedConversationId } = appStore;

  return (
    <PageShell title={t("page.title")}>
      <SearchView
        onNavigateToConversation={(conversationId) => {
          setSelectedConversationId(conversationId);
          navigate({ to: "/dms/$conversationId", params: { conversationId } });
        }}
      />
    </PageShell>
  );
});
