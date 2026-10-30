import { View, Text, Pressable } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { semantic, type as ty } from "../../theme/tokens";

/** A system line where a peer's safety number changed (#1605), Signal-style; the whole line opens "You & <name>". */
export function KeyChangeNoticeRow({
  noticeId,
  peerLabel,
  time,
  onVerify,
}: {
  noticeId: number;
  /** "@name", or undefined while the roster loads. */
  peerLabel?: string;
  time: string;
  onVerify: () => void;
}) {
  const { t } = useTranslation("chat");
  const name = peerLabel ?? t("keyChange.defaultName");
  const text = t("keyChange.notice", { name });
  const link = t("keyChange.verify");
  return (
    <Pressable
      testID={`row-key-change-${noticeId}`}
      accessibilityRole="button"
      accessibilityLabel={`${text} ${link}, ${time}`}
      onPress={onVerify}
      style={{
        paddingHorizontal: 16,
        paddingVertical: 10,
        minHeight: 44,
        alignItems: "center",
        justifyContent: "center",
        gap: 4,
      }}
    >
      <View
        style={{
          flexDirection: "row",
          alignItems: "center",
          justifyContent: "center",
          flexWrap: "wrap",
          columnGap: 6,
        }}
      >
        <Icon.shieldAlert size={16} color={semantic.accent} />
        <Text style={[ty.secondary, { color: semantic.dim, textAlign: "center", flexShrink: 1 }]}>
          {text}
        </Text>
      </View>
      <View style={{ flexDirection: "row", alignItems: "center", columnGap: 8 }}>
        <Text style={[ty.secondary, { color: semantic.accent, textDecorationLine: "underline" }]}>
          {link}
        </Text>
        <Text style={[ty.meta, { color: semantic.muted }]}>{time}</Text>
      </View>
    </Pressable>
  );
}
