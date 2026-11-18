import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { EmptyState } from "../components/ui/EmptyState";
import { TextInput } from "../components/ui/TextInput";
import { BotPairingForm } from "../components/Bots/BotPairingForm";
import { useBotGroup, useCreateBot } from "../hooks/queries/useBots";
import { BOT_PAIR_COMMAND } from "../utils/bots";

/** Add a bot (#1182): name it, run the pairing command on its machine, type the code it prints. */
export const AddBotPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/bots/new" });
  const { group, isAdmin, isLoading } = useBotGroup(groupId);
  const createBot = useCreateBot();
  const [name, setName] = useState("");

  if (isLoading || !group) {
    return null;
  }

  const toBots = () => navigate({ to: "/groups/$groupId/bots", params: { groupId } });

  return (
    <PageShell title={t("bots.create.pageTitle", { group: group.name })} scrollable>
      {!isAdmin ? (
        <EmptyState testId="bots-admin-only">{t("bots.adminOnly")}</EmptyState>
      ) : (
        <BotPairingForm
          testId="add-bot-page"
          leadTitle={t("bots.create.leadTitle")}
          lead={t("bots.create.lead")}
          command={BOT_PAIR_COMMAND}
          ready={name.trim().length > 0}
          submitLabel={t("bots.create.submit")}
          submittingLabel={t("bots.create.submitting")}
          failedLabel={t("bots.create.failed")}
          onSubmit={async (code) => {
            const bot = await createBot.mutateAsync({ groupId, code, displayName: name.trim() });
            navigate({ to: "/groups/$groupId/bots/$botId", params: { groupId, botId: bot.userId } });
          }}
          cancel={{ testId: "add-bot-cancel", onClick: toBots }}
        >
          <TextInput
            data-testid="bot-name"
            label={t("bots.create.nameLabel")}
            value={name}
            onChange={setName}
            disabled={createBot.isPending}
            autoFocus
          />
        </BotPairingForm>
      )}
    </PageShell>
  );
};
