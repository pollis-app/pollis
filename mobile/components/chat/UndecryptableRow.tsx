import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { fonts, semantic, type as ty } from "../../theme/tokens";

// Same columns as MessageRow, so the placeholder lines up with the messages
// around it.
const AVATAR = 40;
const AVATAR_GAP = 12;

/**
 * A message (or a collapsed run, `count`) this device was entitled to read and
 * could not decrypt (#1312), shown so a loss is visible instead of vanishing.
 * Its one control opens Diagnostics; one accessible element, like every
 * message row.
 */
export function UndecryptableRow({
  messageId,
  time,
  count,
  onOpenDiagnostics,
}: {
  messageId: string;
  time: string;
  count: number;
  onOpenDiagnostics: () => void;
}) {
  const { t } = useTranslation("chat");
  const title = t("message.undecryptable.title");
  const body = t("message.undecryptable.body", { count });
  const detail = t("message.undecryptable.detail");
  const link = t("message.undecryptable.diagnostics");
  return (
    <View
      testID={`row-message-${messageId}`}
      accessible
      accessibilityLabel={`${title}, ${time}. ${body} ${detail}`}
      accessibilityHint={link}
      accessibilityActions={[{ name: "activate", label: link }]}
      onAccessibilityAction={(e) => {
        if (e.nativeEvent.actionName === "activate") {
          onOpenDiagnostics();
        }
      }}
      style={{ paddingHorizontal: 16, paddingTop: 10, paddingBottom: 2 }}
    >
      <View style={{ flexDirection: "row", gap: AVATAR_GAP }}>
        <View
          style={{
            width: AVATAR,
            height: AVATAR,
            borderRadius: AVATAR / 2,
            borderWidth: 1,
            borderColor: semantic.edge,
            backgroundColor: semantic.raised,
            alignItems: "center",
            justifyContent: "center",
          }}
        >
          <Icon.lock size={16} color={semantic.muted} />
        </View>
        <View style={{ flex: 1, minWidth: 0, gap: 3 }}>
          <View
            style={{ flexDirection: "row", alignItems: "baseline", flexWrap: "wrap", columnGap: 8 }}
          >
            <Text style={[ty.body, { fontFamily: fonts.semibold, color: semantic.dim }]}>
              {title}
            </Text>
            <Text style={[ty.meta, { color: semantic.muted }]}>{time}</Text>
          </View>
          <Text testID={`text-undecryptable-${messageId}`} style={[ty.body, { color: semantic.muted }]}>
            {body}
          </Text>
          <Text style={[ty.meta, { color: semantic.muted }]}>{detail}</Text>
          <Pressable
            testID={`btn-undecryptable-diagnostics-${messageId}`}
            onPress={onOpenDiagnostics}
            hitSlop={12}
            style={{ alignSelf: "flex-start", minHeight: 24, justifyContent: "center" }}
          >
            <Text style={[ty.meta, { color: semantic.accent, textDecorationLine: "underline" }]}>
              {link}
            </Text>
          </Pressable>
        </View>
      </View>
    </View>
  );
}
