import React from "react";
import { useTranslation } from "react-i18next";
import { PageShell } from "../components/Layout/PageShell";
import { JoinByInvite } from "./JoinByInvite";

/** `/join`: paste an invite code (#847); the deep link is `/invite/$token`. */
export const JoinByInvitePage: React.FC = () => {
  const { t } = useTranslation("channels");

  return (
    <PageShell title={t("joinByInvite.pageTitle")}>
      <JoinByInvite />
    </PageShell>
  );
};
