import React from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { PageColumn } from "../components/ui/PageColumn";
import { NavigableList } from "../components/ui/NavigableList";
import { EmptyState } from "../components/ui/EmptyState";
import { Button } from "../components/ui/Button";
import { useBotGroup, useGroupBots } from "../hooks/queries/useBots";
import { useAutoApproveBotApps } from "../hooks/queries/useBotApps";
import { GROUP_BOT_CAP } from "../utils/bots";
import { errorMessage } from "../utils/errorMessage";

/** Group settings → Bots (#1182): the group's bots, and "Add a bot" until the cap. Admins only. */
export const BotsPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId } = useParams({ from: "/groups/$groupId/bots" });
  const { group, isAdmin, isLoading: groupLoading } = useBotGroup(groupId);
  const { data: bots = [], isLoading, isError, error } = useGroupBots(groupId, isAdmin);
  // #2005: a waiting update that may approve itself lands when an admin opens the list.
  useAutoApproveBotApps(groupId, bots.map((b) => b.userId), isAdmin);

  if (groupLoading || !group) {
    return null;
  }

  const capReached = bots.length >= GROUP_BOT_CAP;
  const openBot = (botId: string) =>
    navigate({ to: "/groups/$groupId/bots/$botId", params: { groupId, botId } });

  return (
    <PageShell title={t("bots.pageTitle", { group: group.name })} scrollable>
      <PageColumn spacing="sections" paneTestId="bots-page">
        <p className="text-body text-muted">{t("bots.intro")}</p>
        {!isAdmin ? (
          <EmptyState background={false} testId="bots-admin-only">
            {t("bots.adminOnly")}
          </EmptyState>
        ) : (
          <div className="flex flex-col gap-4">
            {isError && (
              <p data-testid="bots-error" role="alert" className="text-secondary text-danger">
                {errorMessage(error, t("bots.loadFailed"))}
              </p>
            )}
            <NavigableList
              items={bots}
              isLoading={isLoading}
              emptyLabel={t("bots.empty")}
              testId="bots-list"
              autoFocus={false}
              getKey={(b) => b.userId}
              rowTestId={(b) => `bot-row-${b.userId}`}
              activateRowOn="press"
              onClickRow={(b) => openBot(b.userId)}
              onEnterRow={(b) => openBot(b.userId)}
              renderRow={(b) => (
                <span className="flex min-w-0 flex-1 flex-col">
                  <bdi className="truncate text-fg">{b.displayName}</bdi>
                  <span className="truncate text-secondary text-muted">
                    <bdi>@{b.username}</bdi> · {t("bots.hostCount", { count: b.hostCount })}
                  </span>
                </span>
              )}
            />
            {capReached ? (
              <p data-testid="bots-cap-reached" className="text-secondary text-dim">
                {t("bots.capReached", { count: GROUP_BOT_CAP })}
              </p>
            ) : (
              <div>
                <Button
                  data-testid="bots-add"
                  activateOn="press"
                  onClick={() => navigate({ to: "/groups/$groupId/bots/new", params: { groupId } })}
                >
                  {t("bots.add")}
                </Button>
              </div>
            )}
          </div>
        )}
      </PageColumn>
    </PageShell>
  );
};
