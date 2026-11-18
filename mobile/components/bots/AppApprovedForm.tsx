import { useMemo, useState } from "react";
import { useTranslation } from "react-i18next";
import { View } from "react-native";
import { Button, Group, SectionTitle } from "../ui";
import { ErrorText } from "../groups/FormBits";
import { ToggleRow } from "../self/ToggleRow";
import { AppGrantControls } from "./AppGrantControls";
import { space } from "../../theme/tokens";
import { useApproveBotApp } from "../../hooks/queries";
import {
  EMPTY_DRAFT,
  appApprovalErrorKey,
  buildApproveInput,
  buildGrants,
  grantOptions,
  normalizeGrants,
  type ApprovedAppView,
  type BotAppView,
  type GrantDraft,
} from "../../lib/botApps";

/**
 * Change what the approved App may do, in place (#2005): the same App hash or developer key and
 * manifest, re-signed with the admin's grants (never wider than the manifest), and, for a production
 * App, whether later non-widening updates approve themselves.
 */
export function AppApprovedForm({ view, approved }: { view: BotAppView; approved: ApprovedAppView }) {
  const { t } = useTranslation("mobile");
  const approve = useApproveBotApp(view.groupId, view.botUserId);
  const [draft, setDraft] = useState<GrantDraft>(EMPTY_DRAFT);
  const [autoUpdate, setAutoUpdate] = useState<boolean>(approved.autoUpdate);
  const none = useMemo<ReadonlySet<string>>(() => new Set(), []);
  const options = useMemo(() => grantOptions(view, "approved", none), [view, none]);
  if (!options) {
    return null;
  }
  const grants = buildGrants(options, draft);
  const changed =
    JSON.stringify(grants) !== JSON.stringify(normalizeGrants(approved.grants)) ||
    (!approved.development && autoUpdate !== approved.autoUpdate);
  const busy = approve.isPending;

  const onSave = () => {
    if (busy || !changed) {
      return;
    }
    const input = buildApproveInput(view, "approved", { development: approved.development, grants, autoUpdate });
    if (input) {
      approve.mutate(input, { onSuccess: () => setDraft(EMPTY_DRAFT) });
    }
  };

  return (
    <View testID="section-bot-app-grants" style={{ gap: space.lg }}>
      <SectionTitle style={{ paddingHorizontal: 0 }}>{t("bots.app.grantsHeading")}</SectionTitle>
      <AppGrantControls
        testID="bot-app-grants"
        options={options}
        draft={draft}
        onChange={setDraft}
        disabled={busy}
      />
      {approved.development ? null : (
        <Group>
          <ToggleRow
            testID="toggle-bot-app-auto-update"
            label={t("bots.app.autoUpdateLabel")}
            sub={t("bots.app.autoUpdateHint")}
            on={autoUpdate}
            disabled={busy}
            onToggle={() => setAutoUpdate((v) => !v)}
          />
        </Group>
      )}
      <Button
        full
        variant="primary"
        testID="btn-save-bot-app-grants"
        loading={busy}
        disabled={!changed}
        onPress={onSave}
      >
        {busy ? t("bots.app.saving") : t("bots.app.save")}
      </Button>
      {approve.isError ? (
        <ErrorText testID="text-bot-app-save-error">
          {t(appApprovalErrorKey((approve.error as Error)?.message))}
        </ErrorText>
      ) : null}
    </View>
  );
}
