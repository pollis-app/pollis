import { View, Text, Pressable } from "react-native";
import { Avatar } from "../ui";
import { semantic, type as ty } from "../../theme/tokens";

export function MessageRow({
  av,
  amber,
  name,
  time,
  text,
  pending,
  edited,
  onPressAvatar,
  onLongPress,
  testID,
}: {
  av: string;
  amber?: boolean;
  name: string;
  time: string;
  text?: string;
  pending?: boolean;
  edited?: boolean;
  onPressAvatar?: () => void;
  onLongPress?: () => void;
  testID?: string;
}) {
  return (
    <Pressable
      onLongPress={onLongPress}
      delayLongPress={350}
      testID={testID}
      accessibilityLabel={text ? `${name}: ${text}` : name}
      style={{
        flexDirection: "row",
        gap: 12,
        paddingHorizontal: 18,
        paddingVertical: 8,
        opacity: pending ? 0.55 : 1,
      }}
    >
      <Pressable onPress={onPressAvatar} disabled={!onPressAvatar}>
        <Avatar label={av} variant={amber ? "amber" : "default"} />
      </Pressable>
      <View style={{ flex: 1 }}>
        <View style={{ flexDirection: "row", alignItems: "baseline", gap: 8 }}>
          <Text
            style={{
              fontFamily: ty.h1.fontFamily,
              fontSize: 14,
              color: semantic.ink,
            }}
          >
            {name}
          </Text>
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 11,
              color: semantic.mute,
            }}
          >
            {pending ? "sending…" : time}
          </Text>
        </View>
        {text ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 14,
              lineHeight: 20,
              color: semantic.ink,
              marginTop: 2,
            }}
          >
            {text}
            {edited ? (
              <Text
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 11,
                  color: semantic.mute,
                }}
              >
                {"  (edited)"}
              </Text>
            ) : null}
          </Text>
        ) : null}
      </View>
    </Pressable>
  );
}
