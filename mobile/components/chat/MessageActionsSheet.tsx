import { View, Text, Pressable } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, r } from "../../theme/tokens";
import { SheetOverlay } from "./SheetOverlay";
import type { Message } from "../../hooks/queries";

const QUICK_EMOJI = ["👍", "❤️", "😂", "🎉", "🔥", "🙏"];

/**
 * Long-press action sheet for a message: quick reactions, plus edit/delete
 * for the sender's own messages.
 */
export function MessageActionsSheet({
  target,
  isOwn,
  onReact,
  onOpenPicker,
  onReplyInThread,
  onEdit,
  onDelete,
  onClose,
}: {
  target: Message;
  isOwn: boolean;
  onReact: (emoji: string) => void;
  onOpenPicker: () => void;
  onReplyInThread: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  return (
    <SheetOverlay onClose={onClose}>
      <View
        style={{
          flexDirection: "row",
          justifyContent: "space-between",
          gap: 8,
          paddingVertical: 6,
        }}
      >
        {QUICK_EMOJI.map((emoji, ei) => (
          <Pressable
            key={emoji}
            testID={`btn-react-${ei}`}
            accessibilityRole="button"
            accessibilityLabel={`React ${emoji}`}
            onPress={() => onReact(emoji)}
            style={{
              width: 44,
              height: 44,
              alignItems: "center",
              justifyContent: "center",
              borderWidth: 1,
              borderColor: semantic.hair,
              borderRadius: r.sm,
            }}
          >
            <Text style={{ fontSize: 22 }}>{emoji}</Text>
          </Pressable>
        ))}
        <Pressable
          testID="btn-react-more"
          accessibilityRole="button"
          accessibilityLabel="More reactions"
          onPress={onOpenPicker}
          style={{
            width: 44,
            height: 44,
            alignItems: "center",
            justifyContent: "center",
            borderWidth: 1,
            borderColor: semantic.hairStrong,
            borderRadius: r.sm,
          }}
        >
          <Icon.plus color={semantic.ink} />
        </Pressable>
      </View>

      <Pressable
        onPress={onReplyInThread}
        testID="btn-reply-thread"
        accessibilityRole="button"
        accessibilityLabel="Reply in thread"
        style={{
          paddingVertical: 14,
          paddingHorizontal: 12,
          borderWidth: 1,
          borderColor: semantic.hairStrong,
          borderRadius: r.sm,
          flexDirection: "row",
          alignItems: "center",
          gap: 10,
        }}
      >
        <Icon.thread color={semantic.ink} />
        <Text
          style={{
            fontFamily: ty.body.fontFamily,
            fontSize: 14,
            color: semantic.ink,
          }}
        >
          Reply in thread
        </Text>
      </Pressable>

      {isOwn ? (
        <>
          <Pressable
            onPress={onEdit}
            testID="btn-edit"
            accessibilityRole="button"
            accessibilityLabel="Edit message"
            style={{
              paddingVertical: 14,
              paddingHorizontal: 12,
              borderWidth: 1,
              borderColor: semantic.hairStrong,
              borderRadius: r.sm,
              flexDirection: "row",
              alignItems: "center",
              gap: 10,
            }}
          >
            <Icon.edit color={semantic.ink} />
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 14,
                color: semantic.ink,
              }}
            >
              Edit message
            </Text>
          </Pressable>
          <Pressable
            onPress={onDelete}
            testID="btn-delete"
            accessibilityRole="button"
            accessibilityLabel="Delete message"
            style={{
              paddingVertical: 14,
              paddingHorizontal: 12,
              borderWidth: 1,
              borderColor: "rgba(196,106,46,0.4)",
              borderRadius: r.sm,
              flexDirection: "row",
              alignItems: "center",
              gap: 10,
            }}
          >
            <Icon.exit color={semantic.danger} />
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 14,
                color: semantic.danger,
              }}
            >
              Delete message
            </Text>
          </Pressable>
        </>
      ) : null}

      <Pressable
        onPress={onClose}
        testID="btn-action-cancel"
        accessibilityRole="button"
        accessibilityLabel="Cancel"
        style={{
          paddingVertical: 14,
          alignItems: "center",
        }}
      >
        <Text style={[ty.label, { color: semantic.mute }]}>CANCEL</Text>
      </Pressable>
    </SheetOverlay>
  );
}
