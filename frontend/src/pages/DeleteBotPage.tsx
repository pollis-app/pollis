import React from "react";
import { Trans, useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ConfirmPage } from "../components/Layout/ConfirmPage";
import { botErrorText } from "../components/Bots/botErrorText";
import { useBot, useBotGroup, useDeleteBot } from "../hooks/queries/useBots";

/** Delete a bot (#1182): it leaves the group and every host stops running it. */
export const DeleteBotPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId, botId } = useParams({ from: "/groups/$groupId/bots/$botId/delete" });
  const { isAdmin } = useBotGroup(groupId);
  const { data: bot } = useBot(botId, isAdmin);
  const deleteBot = useDeleteBot();
  const toBot = () => navigate({ to: "/groups/$groupId/bots/$botId", params: { groupId, botId } });

  return (
    <ConfirmPage
      title={t("bots.delete.pageTitle")}
      testId="delete-bot-confirmation"
      question={
        <Trans
          i18nKey="channels:bots.delete.confirm"
          values={{ name: bot?.displayName ?? botId }}
          components={{ strong: <strong /> }}
        />
      }
      consequence={t("bots.delete.consequence")}
      error={deleteBot.isError ? botErrorText(deleteBot.error, t, t("bots.delete.failed")) : null}
      errorTestId="delete-bot-error"
      confirmTestId="delete-bot-confirm"
      confirmLabel={t("bots.delete.submit")}
      pendingLabel={t("bots.delete.submitting")}
      isPending={deleteBot.isPending}
      confirmDisabled={!isAdmin}
      onConfirm={async () => {
        try {
          await deleteBot.mutateAsync({ botUserId: botId, groupId });
          navigate({ to: "/groups/$groupId/bots", params: { groupId } });
        } catch {
          // Shown through `deleteBot.isError` above.
        }
      }}
      cancel={{ testId: "delete-bot-cancel", disabled: deleteBot.isPending, onClick: toBot }}
    />
  );
};
