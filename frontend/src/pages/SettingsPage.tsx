import React from "react";
import { useRouter } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { Settings } from "./Settings";
import type { RouterContext } from "../types/router";

export const SettingsPage: React.FC = () => {
  const router = useRouter();
  const { onDeleteAccount } = router.options.context as RouterContext;

  return (
    <PageShell title="Settings" scrollable>
      <Settings onDeleteAccount={onDeleteAccount} />
    </PageShell>
  );
};
