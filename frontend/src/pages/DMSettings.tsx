import { errorMessage } from "../utils/errorMessage";
import React from "react";
import { Trans, useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";
import { useDMConversations, useLeaveDM } from "../hooks/queries/useMessages";
import { PageShell } from "../components/Layout/PageShell";
import { PageColumn } from "../components/ui/PageColumn";
import { ConfirmBlock } from "../components/ui/ConfirmBlock";
import { ExportArchiveButton } from "../components/Security/ExportArchiveButton";

/** Conversation settings: export it, or leave it (the page's one confirmation, #1968). */
export const DMSettingsPage: React.FC = observer(() => {
  const { t } = useTranslation("dms");
  const { finish } = useFinishFlow();
  const { conversationId } = useParams({ from: "/dms/$conversationId/settings" });
  const { setSelectedConversationId } = appStore;
  const leaveDMMutation = useLeaveDM();
  const { data: conversations = [] } = useDMConversations();
  const peer = conversations.find((c) => c.id === conversationId)?.user2_identifier;

  return (
    <PageShell title={t("settings.pageTitle")}>
      <PageColumn spacing="sections" paneTestId="dm-settings-page">
        <ExportArchiveButton
          conversationId={conversationId}
          fileStem={`pollis-dm-${conversationId}`}
          testId="dm-settings-export-button"
        />
        <ConfirmBlock
          testId="dm-settings-leave"
          question={
            peer ? (
              <Trans
                t={t}
                i18nKey="settings.leaveQuestion"
                values={{ name: `@${peer}` }}
                components={{ strong: <strong /> }}
              />
            ) : (
              t("settings.leaveQuestionUnnamed")
            )
          }
          consequence={t("settings.leaveConsequence")}
          error={
            leaveDMMutation.isError
              ? errorMessage(leaveDMMutation.error, t("settings.leaveFailed"))
              : null
          }
          confirmTestId="dm-settings-leave-button"
          confirmLabel={t("settings.leave")}
          pendingLabel={t("settings.submitting")}
          isPending={leaveDMMutation.isPending}
          onConfirm={async () => {
            try {
              await leaveDMMutation.mutateAsync(conversationId);
              setSelectedConversationId(null);
              finish({ to: "/" });
            } catch {
              // error shown via isError above
            }
          }}
          cancel={{
            testId: "dm-settings-cancel-button",
            label: t("settings.cancel"),
            activateOn: "press",
            // Export keeps the page's first focus, as before; the action never takes it.
            autoFocus: false,
            onClick: () => finish({ to: "/dms/$conversationId", params: { conversationId } }),
          }}
        />
      </PageColumn>
    </PageShell>
  );
});
