import { View, TextInput, Pressable } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, r } from "../../theme/tokens";

/**
 * Bottom composer bar: attach button, text input, send button.
 */
export function Composer({
  draft,
  onChangeDraft,
  onSend,
  sendPending,
  editable,
}: {
  draft: string;
  onChangeDraft: (text: string) => void;
  onSend: () => void;
  sendPending: boolean;
  editable: boolean;
}) {
  return (
    <View
      style={{
        flexDirection: "row",
        alignItems: "center",
        gap: 10,
        paddingVertical: 10,
        paddingHorizontal: 12,
        borderTopWidth: 1,
        borderTopColor: semantic.hairSoft,
      }}
    >
      <Pressable
        testID="btn-attach"
        accessibilityRole="button"
        accessibilityLabel="Add attachment"
        style={{
          width: 38,
          height: 38,
          alignItems: "center",
          justifyContent: "center",
          borderWidth: 1,
          borderColor: semantic.hairStrong,
          borderRadius: r.sm,
        }}
      >
        <Icon.plus color={semantic.ink} />
      </Pressable>
      <TextInput
        testID="input-composer"
        accessibilityLabel="Message"
        value={draft}
        onChangeText={onChangeDraft}
        placeholder="Type a message…"
        placeholderTextColor={semantic.mute}
        onSubmitEditing={onSend}
        returnKeyType="send"
        editable={editable}
        style={{
          flex: 1,
          borderWidth: 1,
          borderColor: semantic.hairStrong,
          borderRadius: r.sm,
          paddingVertical: 10,
          paddingHorizontal: 12,
          fontFamily: ty.body.fontFamily,
          fontSize: 14,
          color: semantic.ink,
          backgroundColor: semantic.fieldBg,
        }}
      />
      <Pressable
        onPress={onSend}
        disabled={!draft.trim() || sendPending}
        testID="btn-send"
        accessibilityRole="button"
        accessibilityLabel="Send"
        style={{
          width: 38,
          height: 38,
          alignItems: "center",
          justifyContent: "center",
          backgroundColor: semantic.accent,
          borderRadius: r.sm,
          opacity: !draft.trim() || sendPending ? 0.4 : 1,
        }}
      >
        <Icon.send color="#0a0907" />
      </Pressable>
    </View>
  );
}
