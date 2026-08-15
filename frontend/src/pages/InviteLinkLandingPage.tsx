import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { JoinByInvite } from "./JoinByInvite";

/**
 * `/invite/$token` — where a shared invite link lands.
 *
 * Parameterized, so it is exempt from the three-place static-page registration
 * (router / PAGE_RESULTS / Sidebar). The discoverable entry point is `/join`.
 */
export const InviteLinkLandingPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { token } = useParams({ from: "/invite/$token" });

  return (
    <PageShell title={t("joinByInvite.pageTitle")}>
      <JoinByInvite initialToken={token} autoRedeem />
    </PageShell>
  );
};
