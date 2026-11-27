import React from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { PageShell } from "../components/Layout/PageShell";
import { EmptyState } from "../components/ui/EmptyState";
import { BotPairingForm } from "../components/Bots/BotPairingForm";
import { useAddBotHost, useBot, useBotGroup } from "../hooks/queries/useBots";
import { botAddHostCommand } from "../utils/bots";

/** Add a host to a bot (#1182): the new machine prints a code; one of the bot's hosts hands it the keys. */
export const AddBotHostPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { finish } = useFinishFlow();
  const { groupId, botId } = useParams({ from: "/groups/$groupId/bots/$botId/add-host" });
  const { group, isAdmin, isLoading } = useBotGroup(groupId);
  const { data: bot } = useBot(botId, isAdmin);
  const addHost = useAddBotHost();

  if (isLoading || !group) {
    return null;
  }

  const name = bot?.displayName ?? botId;
  const toBot = () => finish({ to: "/groups/$groupId/bots/$botId", params: { groupId, botId } });

  return (
    <PageShell title={t("bots.addHost.pageTitle", { bot: name })} scrollable>
      {!isAdmin ? (
        <EmptyState testId="bots-admin-only">{t("bots.adminOnly")}</EmptyState>
      ) : (
        <BotPairingForm
          testId="add-bot-host-page"
          leadTitle={t("bots.addHost.leadTitle")}
          lead={t("bots.addHost.lead", { bot: name })}
          command={botAddHostCommand(botId)}
          submitLabel={t("bots.addHost.submit")}
          submittingLabel={t("bots.addHost.submitting")}
          failedLabel={t("bots.addHost.failed")}
          onSubmit={async (code) => {
            await addHost.mutateAsync({ botUserId: botId, code, groupId });
            toBot();
          }}
          cancel={{ testId: "add-bot-host-cancel", onClick: toBot }}
        />
      )}
    </PageShell>
  );
};
