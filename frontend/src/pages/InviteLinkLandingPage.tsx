import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { JoinByInvite } from "./JoinByInvite";

/** `/invite/$token` landing; parameterized, so not registered as a static page. */
export const InviteLinkLandingPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { token } = useParams({ from: "/invite/$token" });

  return (
    <PageShell title={t("joinByInvite.pageTitle")}>
      <JoinByInvite initialToken={token} />
    </PageShell>
  );
};
