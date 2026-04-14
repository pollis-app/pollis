import React from "react";
import { PageShell } from "../components/Layout/PageShell";
import { AllJoinRequests } from "./AllJoinRequests";

export const AllJoinRequestsPage: React.FC = () => {
  return (
    <PageShell title="Join Requests">
      <AllJoinRequests />
    </PageShell>
  );
};
