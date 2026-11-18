import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { View } from "react-native";
import { Button, Card, Group, SectionTitle } from "../ui";
import { Hint, ErrorText } from "../groups/FormBits";
import { ToggleRow } from "../self/ToggleRow";
import { AppIdentity } from "./AppIdentity";
import { AppPlainList } from "./AppPlainList";
import { AppGrantControls } from "./AppGrantControls";
import { AppChangeList } from "./AppChangeList";
import { space } from "../../theme/tokens";
import { useApproveBotApp } from "../../hooks/queries";
import {
  EMPTY_DRAFT,
  appApprovalErrorKey,
  approvalBlocked,
  buildApproveInput,
  buildGrants,
  grantOptions,
  keyFingerprint,
  type BotAppView,
  type GrantDraft,
  type PendingAppView,
} from "../../lib/botApps";

/**
 * The build the bot's runtime has and an admin may approve (#2005): a first approval, or an update
 * with its diff. Widened access is listed first and only granted when switched on; when nothing
 * widens, Approve update is one tap with the current grants carried over. The development switch
 * approves the developer key instead of this one build; the auto switch lets later non-widening
 * updates from the same source approve themselves.
 */
export function AppPendingForm({ view, pending }: { view: BotAppView; pending: PendingAppView }) {
  const { t } = useTranslation("mobile");
  const approve = useApproveBotApp(view.groupId, view.botUserId);
  const first = !view.approved;
  const devKey = pending.developerKey ?? (view.approved?.development ? view.approved.developerKey : null);

  const [accepted, setAccepted] = useState<ReadonlySet<string>>(new Set());
  const [draft, setDraft] = useState<GrantDraft>(EMPTY_DRAFT);
  const [development, setDevelopment] = useState<boolean>(!!pending.developerKey);
  const [autoUpdate, setAutoUpdate] = useState<boolean>(view.approved?.autoUpdate ?? false);

  const options = useMemo(() => grantOptions(view, "pending", accepted), [view, accepted]);
  if (!options) {
    return null;
  }
  const grants = buildGrants(options, draft);
  const blocked = approvalBlocked(pending.changes, accepted);
  const busy = approve.isPending;

  const toggleAccepted = (key: string) => {
    const next = new Set(accepted);
    if (next.has(key)) {
      next.delete(key);
    } else {
      next.add(key);
    }
    setAccepted(next);
  };

  const onApprove = () => {
    if (busy || blocked) {
      return;
    }
    const input = buildApproveInput(view, "pending", { development: development && !!devKey, grants, autoUpdate });
    if (input) {
      approve.mutate(input);
    }
  };

  return (
    <View testID="section-bot-app-pending" style={{ gap: space.lg }}>
      <SectionTitle style={{ paddingHorizontal: 0 }}>
        {first ? t("bots.app.firstHeading") : t("bots.app.updateHeading")}
      </SectionTitle>
      <Card style={{ gap: space.lg }}>
        <AppIdentity
          testID="bot-app-pending"
          manifest={pending.manifest}
          appHash={pending.appHash}
          developerKey={development && devKey ? devKey : null}
          source={pending.source}
          marks={development && devKey ? ["development"] : []}
        />
      </Card>

      {first ? null : (
        <>
          <SectionTitle style={{ paddingHorizontal: 0 }}>{t("bots.app.changesHeading")}</SectionTitle>
          {pending.changes.length === 0 ? (
            <Hint>{t("bots.app.noChanges")}</Hint>
          ) : (
            <AppChangeList changes={pending.changes} accepted={accepted} onToggle={toggleAccepted} disabled={busy} />
          )}
          {pending.widens ? <Hint>{t("bots.app.notAccepted")}</Hint> : null}
        </>
      )}

      <SectionTitle style={{ paddingHorizontal: 0 }}>{t("bots.app.canDoHeading")}</SectionTitle>
      <Card>
        <AppPlainList testID="list-bot-app-pending-plain" manifest={pending.manifest} grants={grants} />
      </Card>

      <SectionTitle style={{ paddingHorizontal: 0 }}>{t("bots.app.grantsHeading")}</SectionTitle>
      <AppGrantControls
        testID="bot-app-pending-grants"
        options={options}
        draft={draft}
        onChange={setDraft}
        disabled={busy}
      />

      <Group>
        <ToggleRow
          testID="toggle-bot-app-development"
          label={t("bots.app.dev.label")}
          sub={
            devKey
              ? `${t("bots.app.dev.hint")} ${t("bots.app.dev.key", { key: keyFingerprint(devKey) })}`
              : t("bots.app.dev.noKey")
          }
          on={development && !!devKey}
          disabled={busy || !devKey}
          onToggle={() => setDevelopment((v) => !v)}
        />
        {development && devKey ? null : (
          <ToggleRow
            testID="toggle-bot-app-auto-update"
            label={t("bots.app.autoUpdateLabel")}
            sub={t("bots.app.autoUpdateHint")}
            on={autoUpdate}
            disabled={busy}
            onToggle={() => setAutoUpdate((v) => !v)}
          />
        )}
      </Group>

      {blocked ? <Hint>{t("bots.app.mustAccept")}</Hint> : null}
      <Button
        full
        variant="primary"
        testID="btn-approve-bot-app"
        loading={busy}
        disabled={blocked}
        onPress={onApprove}
      >
        {busy ? t("bots.app.approving") : first ? t("bots.app.approve") : t("bots.app.approveUpdate")}
      </Button>
      {approve.isError ? (
        <ErrorText testID="text-bot-app-approve-error">
          {t(appApprovalErrorKey((approve.error as Error)?.message))}
        </ErrorText>
      ) : null}
    </View>
  );
}
