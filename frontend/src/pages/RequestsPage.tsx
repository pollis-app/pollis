import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Requests } from "./Requests";

export const RequestsPage: React.FC = () => {
  const navigate = useNavigate();

  return (
    <PageShell title="Message Requests" onBack={() => navigate({ to: "/dms" })}>
      <Requests />
    </PageShell>
  );
};
