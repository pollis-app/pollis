import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { RenameEntity } from "./RenameEntity";

export const RenameGroupPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/rename" });

  return (
    <PageShell title={t("renameGroup.pageTitle")}>
      <RenameEntity
        kind="group"
        groupId={groupId}
        onSuccess={() => {
          navigate({ to: "/groups/$groupId", params: { groupId } });
        }}
      />
    </PageShell>
  );
};
