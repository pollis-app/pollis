import React from "react";
import { PageShell } from "../components/Layout/PageShell";
import { Blocked } from "./Blocked";

export const BlockedPage: React.FC = () => {
  return (
    <PageShell title="Blocked Users">
      <Blocked />
    </PageShell>
  );
};
