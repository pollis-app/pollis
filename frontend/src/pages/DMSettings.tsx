import { errorMessage } from "../utils/errorMessage";
import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useLeaveDM } from "../hooks/queries/useMessages";
import { Button } from "../components/ui/Button";
import { PageShell } from "../components/Layout/PageShell";

export const DMSettingsPage: React.FC = observer(() => {
  const { t } = useTranslation("dms");
  const navigate = useNavigate();
  const { conversationId } = useParams({ from: "/dms/$conversationId/settings" });
  const { setSelectedConversationId } = appStore;
  const leaveDMMutation = useLeaveDM();

  return (
    <PageShell title={t("settings.pageTitle")}>
      <div className="h-full flex flex-col items-center justify-center gap-4 px-6">
        {leaveDMMutation.isError && (
          <p className="text-xs font-mono text-danger">
            {errorMessage(leaveDMMutation.error, t("settings.leaveFailed"))}
          </p>
        )}
        <Button
          data-testid="dm-settings-leave-button"
          onClick={async () => {
            try {
              await leaveDMMutation.mutateAsync(conversationId);
              setSelectedConversationId(null);
              navigate({ to: "/" });
            } catch {
              // error shown via isError above
            }
          }}
          disabled={leaveDMMutation.isPending}
          isLoading={leaveDMMutation.isPending}
          loadingText={t("settings.submitting")}
          variant="danger"
          className="w-full max-w-[280px]"
        >
          {t("settings.leave")}
        </Button>
        <Button
          data-testid="dm-settings-cancel-button"
          variant="secondary"
          onClick={() => navigate({ to: "/dms/$conversationId", params: { conversationId } })}
          className="w-full max-w-[280px]"
        >
          {t("settings.cancel")}
        </Button>
      </div>
    </PageShell>
  );
});
