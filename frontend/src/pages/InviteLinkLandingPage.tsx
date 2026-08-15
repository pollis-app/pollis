import React from "react";
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
  const { token } = useParams({ from: "/invite/$token" });

  return (
    <PageShell title="Join a Group">
      <JoinByInvite initialToken={token} autoRedeem />
    </PageShell>
  );
};
