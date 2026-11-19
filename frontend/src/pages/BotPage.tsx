import React, { useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { PageShell } from "../components/Layout/PageShell";
import { PageColumn } from "../components/ui/PageColumn";
import { FormLead } from "../components/ui/FormLead";
import { NavigableList } from "../components/ui/NavigableList";
import { EmptyState } from "../components/ui/EmptyState";
import { ConfirmBlock } from "../components/ui/ConfirmBlock";
import { Button } from "../components/ui/Button";
import { BotBadge } from "../components/ui/BotBadge";
import { botErrorText } from "../components/Bots/botErrorText";
import { BotAppSection } from "../components/Bots/BotAppSection";
import { useBot, useBotGroup, useRevokeBotHost } from "../hooks/queries/useBots";
import { formatDateTime } from "../utils/format";
import type { BotHost } from "../types";

/**
 * One bot (#1182): its App and its approval (#2005, `BotAppSection`), its hosts (name, last seen, revoke), "Add a host", and delete. Revoke confirms
 * in place of the list (the Security page's device pattern); delete is its own confirmation page.
 */
export const BotPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId, botId } = useParams({ from: "/groups/$groupId/bots/$botId" });
  const { group, isAdmin, isLoading: groupLoading } = useBotGroup(groupId);
  const { data: bot, isLoading, isError } = useBot(botId, isAdmin);
  const revokeHost = useRevokeBotHost();
  const [confirming, setConfirming] = useState<BotHost | null>(null);

  if (groupLoading || !group) {
    return null;
  }

  const title = bot?.displayName ?? t("nav:breadcrumb.bot");
  const hosts = bot?.hosts ?? [];
  const isLastHost = hosts.length === 1;

  const cancelRevoke = () => {
    setConfirming(null);
    revokeHost.reset();
  };

  const revoke = async () => {
    if (!confirming) {
      return;
    }
    try {
      await revokeHost.mutateAsync({ botUserId: botId, deviceId: confirming.deviceId, groupId });
      setConfirming(null);
    } catch {
      // Shown through `revokeHost.isError` below.
    }
  };

  let body: React.ReactNode;
  if (!isAdmin) {
    body = <EmptyState background={false} testId="bots-admin-only">{t("bots.adminOnly")}</EmptyState>;
  } else if (isLoading) {
    body = <EmptyState background={false}>{t("common:states.loading")}</EmptyState>;
  } else if (isError || !bot) {
    body = <EmptyState background={false} testId="bot-not-found">{t("bots.notFound")}</EmptyState>;
  } else {
    body = (
      <>
        <div className="flex items-center gap-2">
          <bdi data-testid="bot-handle" className="truncate text-body text-dim">
            @{bot.username}
          </bdi>
          <BotBadge groupName={group.name} data-testid="bot-page-badge" />
        </div>

        <BotAppSection groupId={groupId} botUserId={botId} />

        <section className="flex flex-col gap-4" data-testid="bot-hosts">
          <FormLead title={t("bots.detail.hostsHeading")}>{t("bots.detail.hostsDescription")}</FormLead>
          {confirming ? (
            <ConfirmBlock
              testId="revoke-host-confirm"
              question={
                <Trans
                  t={t}
                  i18nKey="bots.detail.revokeConfirm"
                  values={{ host: confirming.name }}
                  components={{ name: <strong /> }}
                />
              }
              consequence={
                isLastHost ? t("bots.detail.revokeLastConsequence") : t("bots.detail.revokeConsequence")
              }
              error={
                revokeHost.isError
                  ? botErrorText(revokeHost.error, t, t("bots.detail.revokeFailed"))
                  : null
              }
              errorTestId="revoke-host-error"
              confirmTestId="revoke-host-submit"
              confirmLabel={t("bots.detail.revokeSubmit")}
              pendingLabel={t("bots.detail.revoking")}
              isPending={revokeHost.isPending}
              onConfirm={revoke}
              cancel={{ testId: "revoke-host-cancel", disabled: revokeHost.isPending, onClick: cancelRevoke }}
            />
          ) : (
            <NavigableList<BotHost>
              testId="bot-hosts-list"
              autoFocus={false}
              items={hosts}
              emptyLabel={t("bots.detail.hostsEmpty")}
              getKey={(h) => h.deviceId}
              rowTestId={(h) => `bot-host-${h.deviceId}`}
              renderRow={(h) => (
                <span className="flex min-w-0 flex-col">
                  <bdi className="truncate text-fg">{h.name}</bdi>
                  <span className="text-secondary text-dim">
                    {h.lastSeenAt
                      ? t("bots.detail.lastSeen", { time: formatDateTime(h.lastSeenAt) })
                      : t("bots.detail.neverSeen")}
                  </span>
                </span>
              )}
              controls={(h) => [
                <Button
                  key="revoke"
                  size="sm"
                  variant="danger"
                  data-testid={`bot-host-revoke-${h.deviceId}`}
                  onClick={() => {
                    revokeHost.reset();
                    setConfirming(h);
                  }}
                >
                  {t("bots.detail.revoke")}
                </Button>,
              ]}
            />
          )}
          <p data-testid="bot-recovery-note" className="text-secondary text-dim">
            {t("bots.detail.recoveryNote")}
          </p>
          <div>
            <Button
              variant="secondary"
              data-testid="bot-add-host"
              activateOn="press"
              disabled={!!confirming}
              onClick={() => navigate({ to: "/groups/$groupId/bots/$botId/add-host", params: { groupId, botId } })}
            >
              {t("bots.detail.addHost")}
            </Button>
          </div>
        </section>

        <section className="flex flex-col gap-4">
          <FormLead title={t("bots.detail.deleteHeading")}>{t("bots.detail.deleteDescription")}</FormLead>
          <div>
            <Button
              variant="danger"
              data-testid="bot-delete"
              disabled={!!confirming}
              onClick={() => navigate({ to: "/groups/$groupId/bots/$botId/delete", params: { groupId, botId } })}
            >
              {t("bots.detail.delete")}
            </Button>
          </div>
        </section>
      </>
    );
  }

  return (
    <PageShell title={title} scrollable>
      <PageColumn spacing="sections" paneTestId="bot-page">
        {body}
      </PageColumn>
    </PageShell>
  );
};
