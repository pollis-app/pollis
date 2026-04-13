import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Blocked } from "./Blocked";

export const BlockedPage: React.FC = () => {
  const navigate = useNavigate();

  return (
    <PageShell title="Blocked Users" onBack={() => navigate({ to: "/dms" })}>
      <Blocked />
    </PageShell>
  );
};
