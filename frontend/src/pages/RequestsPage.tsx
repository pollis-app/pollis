import React from "react";
import { PageShell } from "../components/Layout/PageShell";
import { Requests } from "./Requests";

export const RequestsPage: React.FC = () => {
  return (
    <PageShell title="Message Requests">
      <Requests />
    </PageShell>
  );
};
