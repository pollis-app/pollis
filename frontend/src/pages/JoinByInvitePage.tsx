import React from "react";
import { PageShell } from "../components/Layout/PageShell";
import { JoinByInvite } from "./JoinByInvite";

/**
 * `/join` — paste an invite code. The discoverable half of #847; the other
 * surface is the `/invite/$token` deep link.
 */
export const JoinByInvitePage: React.FC = () => {
  return (
    <PageShell title="Join a Group">
      <JoinByInvite />
    </PageShell>
  );
};
