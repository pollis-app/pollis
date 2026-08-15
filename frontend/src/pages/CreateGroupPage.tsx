import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { CreateGroup } from "./CreateGroup";

export const CreateGroupPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();

  return (
    <PageShell title={t("createGroup.pageTitle")} scrollable>
      <CreateGroup onSuccess={(groupId) => navigate({ to: "/groups/$groupId", params: { groupId } })} />
    </PageShell>
  );
};
