import React from "react";
import { useTranslation } from "react-i18next";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { PageShell } from "../components/Layout/PageShell";
import { CreateGroup } from "./CreateGroup";

export const CreateGroupPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { finish } = useFinishFlow();

  return (
    <PageShell title={t("createGroup.pageTitle")} scrollable>
      <CreateGroup onSuccess={(groupId) => finish({ to: "/groups/$groupId", params: { groupId } })} />
    </PageShell>
  );
};
