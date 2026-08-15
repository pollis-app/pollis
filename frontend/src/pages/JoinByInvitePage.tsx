import React from "react";
import { useTranslation } from "react-i18next";
import { PageShell } from "../components/Layout/PageShell";
import { JoinByInvite } from "./JoinByInvite";

/**
 * `/join` — paste an invite code. The discoverable half of #847; the other
 * surface is the `/invite/$token` deep link.
 */
export const JoinByInvitePage: React.FC = () => {
  const { t } = useTranslation("channels");

  return (
    <PageShell title={t("joinByInvite.pageTitle")}>
      <JoinByInvite />
    </PageShell>
  );
};
