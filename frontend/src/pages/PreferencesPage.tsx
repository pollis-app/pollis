import React from "react";
import { useNavigate } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Preferences } from "./Preferences";

export const PreferencesPage: React.FC = () => {
  const navigate = useNavigate();

  return (
    <PageShell title="Preferences" onBack={() => navigate({ to: "/" })} scrollable>
      <Preferences />
    </PageShell>
  );
};
