import React from "react";
import { useNavigate, useRouter } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Settings } from "./Settings";
import type { RouterContext } from "../types/router";

export const SettingsPage: React.FC = () => {
  const navigate = useNavigate();
  const router = useRouter();
  const { onDeleteAccount } = router.options.context as RouterContext;

  return (
    <PageShell title="Settings" onBack={() => navigate({ to: "/" })} scrollable>
      <Settings onDeleteAccount={onDeleteAccount} />
    </PageShell>
  );
};
