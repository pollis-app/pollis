import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { PageShell } from "../components/Layout/PageShell";
import { RenameEntity } from "./RenameEntity";

export const RenameGroupPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { finish } = useFinishFlow();
  const { groupId } = useParams({ from: "/groups/$groupId/rename" });

  return (
    <PageShell title={t("renameGroup.pageTitle")}>
      <RenameEntity
        kind="group"
        groupId={groupId}
        onSuccess={() => {
          finish({ to: "/groups/$groupId", params: { groupId } });
        }}
      />
    </PageShell>
  );
};
