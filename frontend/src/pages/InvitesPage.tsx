import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Invites } from "./Invites";

export const InvitesPage: React.FC = () => {
  const navigate = useNavigate();

  return (
    <PageShell title="Invites" onBack={() => navigate({ to: "/" })}>
      <Invites />
    </PageShell>
  );
};
