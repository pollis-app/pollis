import React from "react";
import { useTranslation } from "react-i18next";
import { useBotApp } from "../../hooks/queries/useBotApps";
import { useGroupMembers } from "../../hooks/queries/useGroups";
import { useGroupAliases } from "../../hooks/queries/useGroupAliases";
import { aliasName } from "../../utils/groupAlias";
import { errorMessage } from "../../utils/errorMessage";
import { EmptyState } from "../ui/EmptyState";
import { AppNotice } from "./AppNotice";
import { ApprovedAppSection } from "./ApprovedAppSection";
import { PendingAppSection } from "./PendingAppSection";

interface BotAppSectionProps {
  groupId: string;
  botUserId: string;
}

/**
 * A bot's App on its admin page (#2005), in order: why it isn't running, the approved App and
 * its access, then the build waiting for approval. Updates set to approve themselves are taken
 * by the bot's runtime from the approved source; the page only shows what runs.
 */
export const BotAppSection: React.FC<BotAppSectionProps> = ({ groupId, botUserId }) => {
  const { t } = useTranslation("channels");
  const { data: view, isLoading, isError, error } = useBotApp(groupId, botUserId, true);
  const { data: members = [] } = useGroupMembers(groupId);
  const aliases = useGroupAliases(groupId);

  if (isLoading) {
    return <EmptyState background={false}>{t("common:states.loading")}</EmptyState>;
  }
  if (isError || !view) {
    return (
      <p data-testid="bot-app-error" role="alert" className="text-secondary text-danger">
        {errorMessage(error, t("bots.app.loadFailed"))}
      </p>
    );
  }

  const approverName = (userId: string) => {
    const m = members.find((x) => x.user_id === userId);
    return aliasName(aliases, userId, m?.display_name || m?.username || userId);
  };

  return (
    <div className="flex flex-col gap-8" data-testid="bot-app">
      <AppNotice view={view} />
      {view.approved && (
        <ApprovedAppSection
          key={`approved-${view.approved.seq}`}
          view={view}
          approved={view.approved}
          approverName={approverName(view.approved.approverUserId)}
          editable={!view.pending}
        />
      )}
      {view.pending && (
        <PendingAppSection
          key={`pending-${view.pending.appHash}-${view.pending.manifestHash}-${view.latestSeq}`}
          view={view}
          pending={view.pending}
        />
      )}
      {!view.approved && !view.pending && (
        <EmptyState background={false} testId="bot-app-none">
          {t("bots.app.noApp")}
        </EmptyState>
      )}
    </div>
  );
};
