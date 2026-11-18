import { useTranslation } from "react-i18next";
import { View } from "react-native";
import { Group, Field } from "../ui";
import { ToggleRow } from "../self/ToggleRow";
import { LabeledField, Hint } from "../groups/FormBits";
import { space } from "../../theme/tokens";
import {
  channelOn,
  draftHistory,
  egressOn,
  parseHistoryInput,
  type GrantDraft,
  type GrantOptions,
} from "../../lib/botApps";

/**
 * What the admin allows (#2005): a switch per channel the App asks for, the days of earlier messages
 * it may read (0 up to what it asks), a switch per host it may send data to. Only narrowing is
 * possible: every option comes from the manifest (or, on an update, from what is carried over plus
 * the new access switched on above), so no value here can be wider than the App asks for.
 */
export function AppGrantControls({
  options,
  draft,
  onChange,
  disabled,
  testID,
}: {
  options: GrantOptions;
  draft: GrantDraft;
  onChange: (draft: GrantDraft) => void;
  disabled?: boolean;
  testID: string;
}) {
  const { t } = useTranslation("mobile");
  const history = draftHistory(options, draft);
  return (
    <View testID={testID} style={{ gap: space.lg }}>
      <Hint>{t("bots.app.grantsHint")}</Hint>
      {options.channels.length === 0 ? (
        <Hint>{t("bots.app.noChannels")}</Hint>
      ) : (
        <Group title={t("bots.app.channelsLabel")}>
          {options.channels.map((c) => {
            const on = channelOn(options, draft, c.id);
            const flip = () => onChange({ ...draft, channels: { ...draft.channels, [c.id]: !on } });
            return (
              <ToggleRow
                key={c.id}
                testID={`${testID}-toggle-channel-${c.name}`}
                label={`#${c.name}`}
                on={on}
                disabled={disabled}
                onToggle={flip}
              />
            );
          })}
        </Group>
      )}
      <LabeledField label={t("bots.app.historyLabel", { max: options.maxHistory })}>
        <Field
          testID={`${testID}-history`}
          accessibilityLabel={t("bots.app.historyLabel", { max: options.maxHistory })}
          keyboardType="number-pad"
          editable={!disabled && options.maxHistory > 0}
          value={String(history)}
          onChangeText={(text) => {
            const n = parseHistoryInput(text);
            if (n !== null) {
              onChange({ ...draft, history: n });
            }
          }}
        />
      </LabeledField>
      {options.egress.length > 0 ? (
        <Group title={t("bots.app.egressLabel")}>
          {options.egress.map((host) => {
            const on = egressOn(options, draft, host);
            const flip = () => onChange({ ...draft, egress: { ...draft.egress, [host]: !on } });
            return (
              <ToggleRow
                key={host}
                testID={`${testID}-toggle-egress-${host}`}
                label={host}
                on={on}
                disabled={disabled}
                onToggle={flip}
              />
            );
          })}
        </Group>
      ) : null}
    </View>
  );
}
