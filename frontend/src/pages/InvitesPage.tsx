import React from "react";
import { PageShell } from "../components/Layout/PageShell";
import { Invites } from "./Invites";

export const InvitesPage: React.FC = () => {
  return (
    <PageShell title="Invites">
      <Invites />
    </PageShell>
  );
};
