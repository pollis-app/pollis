import React from "react";
import { PageShell } from "../components/Layout/PageShell";
import { Preferences } from "./Preferences";

export const PreferencesPage: React.FC = () => {
  return (
    <PageShell title="Preferences" scrollable>
      <Preferences />
    </PageShell>
  );
};
