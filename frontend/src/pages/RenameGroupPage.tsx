import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { RenameGroup } from "./RenameGroup";

export const RenameGroupPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/rename" });

  return (
    <PageShell title={t("renameGroup.pageTitle")}>
      <RenameGroup
        groupId={groupId}
        onSuccess={() => {
          navigate({ to: "/groups/$groupId", params: { groupId } });
        }}
      />
    </PageShell>
  );
};
