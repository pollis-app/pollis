import React from "react";
import { PageShell } from "../components/Layout/PageShell";
import { Settings } from "./Settings";

export const SettingsPage: React.FC = () => {
  return (
    <PageShell title="User Settings" scrollable>
      <Settings />
    </PageShell>
  );
};
