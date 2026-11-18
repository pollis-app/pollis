import { useTranslation } from "react-i18next";
import { Pressable, View } from "react-native";
import { Text } from "../Text";
import { Tag } from "../Tag";
import { Toggle } from "../ui";
import { semantic, type as ty, space, r, layout } from "../../theme/tokens";
import { haptic } from "../../lib/haptics";
import { changeKey, changeText, sortChanges, type ManifestChange } from "../../lib/botApps";

/**
 * An update's diff against the approved App (#2005). New access is listed first, each row
 * highlighted (a solid tinted fill and edge, and the words "New access") with its own switch, OFF
 * until the admin turns it on: nothing widened is ever pre-accepted. Rows that only take access
 * away are listed after, plain.
 */
export function AppChangeList({
  changes,
  accepted,
  onToggle,
  disabled,
}: {
  changes: readonly ManifestChange[];
  accepted: ReadonlySet<string>;
  onToggle: (key: string) => void;
  disabled?: boolean;
}) {
  const { t } = useTranslation("mobile");
  const tr = (key: string, options?: Record<string, unknown>) => t(key, options) as string;
  return (
    <View testID="list-bot-app-changes" style={{ gap: space.sm }}>
      {sortChanges(changes).map((c) => {
        const key = changeKey(c);
        const text = changeText(tr, c);
        if (!c.widening) {
          return (
            <View
              key={key}
              testID={`row-bot-app-change-${c.field}-${c.value}`}
              style={{ paddingHorizontal: space.xxl, paddingVertical: space.sm }}
            >
              <Text style={[ty.secondary, { color: semantic.dim }]}>{text}</Text>
            </View>
          );
        }
        const on = accepted.has(key);
        const flip = () => {
          haptic("toggle");
          onToggle(key);
        };
        return (
          <Pressable
            key={key}
            testID={`row-bot-app-widening-${c.field}-${c.value}`}
            onPress={disabled ? undefined : flip}
            disabled={disabled}
            accessibilityRole="switch"
            accessibilityLabel={`${t("bots.app.newAccess")}, ${text}`}
            accessibilityState={{ checked: on, disabled: !!disabled }}
            style={{
              flexDirection: "row",
              alignItems: "center",
              gap: space.xl,
              minHeight: Math.max(layout.touchMin, 56),
              paddingVertical: space.md,
              paddingStart: space.xxl,
              paddingEnd: space.xl,
              borderRadius: r.lg,
              borderWidth: 1,
              borderColor: semantic.accent,
              backgroundColor: semantic.accentFaint,
            }}
          >
            <View style={{ flex: 1, minWidth: 0, gap: space.xs }}>
              <View style={{ flexDirection: "row" }}>
                <Tag tone="accent" label={t("bots.app.newAccess")} />
              </View>
              <Text style={[ty.body, { color: semantic.text }]}>{text}</Text>
            </View>
            <View importantForAccessibility="no-hide-descendants" accessibilityElementsHidden>
              <Toggle
                on={on}
                disabled={disabled}
                testID={`toggle-bot-app-widening-${c.field}-${c.value}`}
                onPress={disabled ? undefined : () => onToggle(key)}
              />
            </View>
          </Pressable>
        );
      })}
    </View>
  );
}
