import React, { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import type { BotAppView, PendingAppView } from "../../types";
import {
  NO_NARROWING,
  appApprovalErrorKind,
  applyNarrowing,
  approveInput,
  changeKey,
  developerKeyFor,
  grantCeiling,
  requiredWidenings,
  type GrantNarrowing,
} from "../../utils/botApps";
import { useApproveBotApp, useRefetchBotApp } from "../../hooks/queries/useBotApps";
import { FormLead } from "../ui/FormLead";
import { FormActions } from "../ui/FormActions";
import { Button } from "../ui/Button";
import { Switch } from "../ui/Switch";
import { AppSummary } from "./AppSummary";
import { AppMark } from "./AppMark";
import { AppCapabilities } from "./AppCapabilities";
import { AppChangeList } from "./AppChangeList";
import { GrantControls } from "./GrantControls";
import { approvalErrorText } from "./appText";

interface PendingAppSectionProps {
  view: BotAppView;
  pending: PendingAppView;
}

/**
 * The build waiting for approval (#2005): a first approval, or an update with its diff. A
 * widening is never pre-accepted; with none, the update approves in one tap with the current
 * grants carried over. Keyed by the build at the call site, so a new build starts clean.
 */
export const PendingAppSection: React.FC<PendingAppSectionProps> = ({ view, pending }) => {
  const { t } = useTranslation("channels");
  const approve = useApproveBotApp();
  const refetch = useRefetchBotApp();
  const isUpdate = view.approved !== null;
  const developerKey = developerKeyFor(view);

  const [accepted, setAccepted] = useState<ReadonlySet<string>>(new Set());
  const [narrowing, setNarrowing] = useState<GrantNarrowing>(NO_NARROWING);
  const [development, setDevelopment] = useState(
    pending.developerKey !== null || (view.approved?.development ?? false),
  );
  const [autoUpdate, setAutoUpdate] = useState(view.approved?.autoUpdate ?? false);

  const ceiling = useMemo(
    () => grantCeiling(pending.suggestedGrants, pending.changes, accepted, pending.manifest, view.channels),
    [pending, accepted, view.channels],
  );
  const grants = applyNarrowing(ceiling, narrowing);
  const missing = requiredWidenings(pending.changes).filter((c) => !accepted.has(changeKey(c)));
  const devOn = development && developerKey !== null;
  const busy = approve.isPending;

  const toggleWidening = (key: string, on: boolean) => {
    setAccepted((prev) => {
      const next = new Set(prev);
      if (on) {
        next.add(key);
      } else {
        next.delete(key);
      }
      return next;
    });
  };

  const submit = async () => {
    try {
      await approve.mutateAsync(approveInput({ view, of: "pending", development: devOn, autoUpdate, grants }));
    } catch (err) {
      // A stale or changed page reloads; the error says why.
      if (appApprovalErrorKind(err) !== null) {
        void refetch(view.groupId, view.botUserId);
      }
    }
  };

  return (
    <section className="flex flex-col gap-5" data-testid="bot-app-pending" data-update={isUpdate ? "true" : "false"}>
      <FormLead title={isUpdate ? t("bots.app.pending.updateHeading") : t("bots.app.pending.firstHeading")}>
        {isUpdate ? t("bots.app.pending.updateLead") : t("bots.app.pending.firstLead")}
      </FormLead>

      <AppSummary
        testId="bot-app-pending-summary"
        manifest={pending.manifest}
        appHash={pending.appHash}
        developerKey={pending.developerKey}
        source={pending.source}
        marks={
          pending.developerKey ? (
            <AppMark label={t("bots.app.marks.development")} data-testid="bot-app-pending-development-mark" />
          ) : null
        }
      />

      {isUpdate &&
        (pending.changes.length > 0 ? (
          <AppChangeList changes={pending.changes} accepted={accepted} onToggle={toggleWidening} disabled={busy} />
        ) : (
          <p data-testid="bot-app-nothing-new" className="text-body text-dim">
            {t("bots.app.pending.nothingNew")}
          </p>
        ))}

      <GrantControls
        testId="bot-app-pending-grants"
        ceiling={ceiling}
        narrowing={narrowing}
        onChange={setNarrowing}
        disabled={busy}
      />

      <AppCapabilities testId="bot-app-pending-can" manifest={pending.manifest} grants={grants} />

      <Switch
        id="bot-app-development"
        data-testid="bot-app-development"
        label={t("bots.app.pending.development")}
        description={
          developerKey ? t("bots.app.pending.developmentHint") : t("bots.app.pending.developmentUnavailable")
        }
        checked={devOn}
        disabled={busy || developerKey === null}
        onChange={setDevelopment}
      />

      {!devOn && (
        <Switch
          id="bot-app-auto-update"
          data-testid="bot-app-auto-update"
          label={t("bots.app.pending.autoUpdate")}
          description={t("bots.app.pending.autoUpdateHint")}
          checked={autoUpdate}
          disabled={busy}
          onChange={setAutoUpdate}
        />
      )}

      {missing.length > 0 && (
        <p data-testid="bot-app-needed-hint" className="text-secondary text-muted">
          {t("bots.app.pending.neededHint")}
        </p>
      )}
      {approve.isError && (
        <p data-testid="bot-app-approve-error" role="alert" className="text-secondary text-danger">
          {approvalErrorText(t, approve.error)}
        </p>
      )}
      <FormActions>
        <Button
          data-testid="bot-app-approve"
          onClick={submit}
          disabled={busy || missing.length > 0}
          isLoading={busy}
          loadingText={t("bots.app.pending.approving")}
        >
          {isUpdate ? t("bots.app.pending.approveUpdate") : t("bots.app.pending.approve")}
        </Button>
      </FormActions>
    </section>
  );
};
