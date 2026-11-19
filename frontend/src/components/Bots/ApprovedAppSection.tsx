import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { ApprovedAppView, BotAppView } from "../../types";
import {
  appApprovalErrorKind,
  applyNarrowing,
  approveInput,
  fullGrants,
  narrowingFrom,
  sameGrants,
  type GrantNarrowing,
} from "../../utils/botApps";
import { useApproveBotApp, useRefetchBotApp } from "../../hooks/queries/useBotApps";
import { activeLocale } from "../../utils/format";
import { FormActions } from "../ui/FormActions";
import { Button } from "../ui/Button";
import { Switch } from "../ui/Switch";
import { CodeSnippet } from "../ui/CodeSnippet";
import { AppSummary } from "./AppSummary";
import { AppMark } from "./AppMark";
import { AppCapabilities } from "./AppCapabilities";
import { GrantControls } from "./GrantControls";
import { approvalErrorText } from "./appText";

interface ApprovedAppSectionProps {
  view: BotAppView;
  approved: ApprovedAppView;
  approverName: string;
  /** Off while an update waits: approving that is the way to change access then. */
  editable: boolean;
}

/**
 * The approved App (#2005): what it is, who approved it, what it can do, and the admin's
 * controls to narrow its access in place (a new approval of the same build).
 */
export const ApprovedAppSection: React.FC<ApprovedAppSectionProps> = ({ view, approved, approverName, editable }) => {
  const { t } = useTranslation("channels");
  const approve = useApproveBotApp();
  const refetch = useRefetchBotApp();

  // Everything the approved manifest asks for, among the group's channels and those granted now.
  const ceiling = useMemo(
    () => fullGrants(approved.manifest, [...view.channels, ...approved.grants.channels]),
    [approved, view.channels],
  );
  const [narrowing, setNarrowing] = useState<GrantNarrowing>(() => narrowingFrom(ceiling, approved.grants));
  const [autoUpdate, setAutoUpdate] = useState(approved.autoUpdate);
  const grants = applyNarrowing(ceiling, narrowing);
  const changed = !sameGrants(grants, approved.grants) || autoUpdate !== approved.autoUpdate;
  const busy = approve.isPending;

  const save = async () => {
    try {
      await approve.mutateAsync(approveInput({ view, of: "approved", development: approved.development, autoUpdate, grants }));
    } catch (err) {
      if (appApprovalErrorKind(err) !== null) {
        void refetch(view.groupId, view.botUserId);
      }
    }
  };

  const issued = new Date(approved.issuedAt * 1000).toLocaleString(activeLocale());

  return (
    <section className="flex flex-col gap-5" data-testid="bot-app-approved">
      <h2 className="text-title text-fg">{t("bots.app.approvedHeading")}</h2>
      <AppSummary
        testId="bot-app-approved-summary"
        manifest={approved.manifest}
        appHash={approved.appHash}
        developerKey={approved.developerKey}
        source={approved.source}
        marks={
          <>
            {approved.development && (
              <AppMark label={t("bots.app.marks.development")} data-testid="bot-app-development-mark" />
            )}
            {approved.cannotSendOutside && (
              <AppMark label={t("bots.app.marks.noEgress")} data-testid="bot-app-no-egress-mark" />
            )}
            {approved.autoUpdate && (
              <AppMark label={t("bots.app.marks.autoUpdate")} data-testid="bot-app-auto-update-mark" />
            )}
          </>
        }
        footer={
          <p data-testid="bot-app-approved-by" className="text-secondary text-muted">
            {t("bots.app.approvedBy", { name: approverName, date: issued })} · {t("bots.app.seq", { seq: approved.seq })}
          </p>
        }
      />
      {view.running?.autoUpdated && (
        <div className="flex flex-col gap-1" data-testid="bot-app-running">
          <p data-testid="bot-app-running-version" className="text-body text-dim">
            {t("bots.app.runningAutoUpdated", { version: view.running.version })}
          </p>
          <CodeSnippet
            value={view.running.appHash}
            data-testid="bot-app-running-hash"
            copyTestId="bot-app-running-hash-copy"
          />
        </div>
      )}
      <AppCapabilities testId="bot-app-approved-can" manifest={approved.manifest} grants={approved.grants} />

      {editable && (
        <div className="flex flex-col gap-4" data-testid="bot-app-edit">
          <GrantControls
            testId="bot-app-edit-grants"
            ceiling={ceiling}
            narrowing={narrowing}
            onChange={setNarrowing}
            disabled={busy}
          />
          {!approved.development && (
            <Switch
              id="bot-app-edit-auto-update"
              data-testid="bot-app-edit-auto-update"
              label={t("bots.app.pending.autoUpdate")}
              description={t("bots.app.pending.autoUpdateHint")}
              checked={autoUpdate}
              disabled={busy}
              onChange={setAutoUpdate}
            />
          )}
          {approve.isError && (
            <p data-testid="bot-app-edit-error" role="alert" className="text-secondary text-danger">
              {approvalErrorText(t, approve.error)}
            </p>
          )}
          <FormActions>
            <Button
              variant="secondary"
              data-testid="bot-app-edit-save"
              onClick={save}
              disabled={busy || !changed}
              isLoading={busy}
              loadingText={t("bots.app.edit.saving")}
            >
              {t("bots.app.edit.save")}
            </Button>
          </FormActions>
        </div>
      )}
    </section>
  );
};
