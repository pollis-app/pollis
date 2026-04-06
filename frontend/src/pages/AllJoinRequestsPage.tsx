import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { AllJoinRequests } from "./AllJoinRequests";

export const AllJoinRequestsPage: React.FC = () => {
  const navigate = useNavigate();

  return (
    <PageShell title="Join Requests" onBack={() => navigate({ to: "/" })}>
      <AllJoinRequests />
    </PageShell>
  );
};
