import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { GroupEmoji } from "./GroupEmoji";

export const GroupEmojiPage: React.FC = () => {
  const { t } = useTranslation("emoji");
  const { groupId } = useParams({ from: "/groups/$groupId/emoji" });

  return (
    <PageShell title={t("manage.title")} scrollable>
      <GroupEmoji groupId={groupId} />
    </PageShell>
  );
};
