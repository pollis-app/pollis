import { useEffect, useRef, useState } from "react";
import { View, Text, Pressable } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, r } from "../../theme/tokens";
import { SheetOverlay } from "./SheetOverlay";
import type { Message } from "../../hooks/queries";

const QUICK_EMOJI = ["👍", "❤️", "😂", "🎉", "🔥", "🙏"];

// #897: copy feedback is verified — the state comes from the clipboard
// call's boolean, never assumed. Matches desktop's 2s reset.
type CopyState = "idle" | "copied" | "failed";
const COPY_FEEDBACK_MS = 2000;

function ActionButton({
  icon,
  label,
  tone = "default",
  onPress,
  testID,
  copyState,
}: {
  icon: React.ReactNode;
  label: string;
  tone?: "default" | "accent" | "danger";
  onPress: () => void;
  testID?: string;
  copyState?: CopyState;
}) {
  const color =
    tone === "danger"
      ? semantic.danger
      : tone === "accent"
        ? semantic.accent
        : semantic.ink;
  return (
    <Pressable
      onPress={onPress}
      testID={testID}
      accessibilityRole="button"
      accessibilityLabel={label}
      accessibilityValue={copyState ? { text: copyState } : undefined}
      style={{
        paddingVertical: 14,
        paddingHorizontal: 12,
        borderWidth: 1,
        borderColor:
          tone === "danger" ? "rgba(196,106,46,0.4)" : semantic.hairStrong,
        borderRadius: r.sm,
        flexDirection: "row",
        alignItems: "center",
        gap: 10,
      }}
    >
      {icon}
      <Text
        style={{
          fontFamily: ty.body.fontFamily,
          fontSize: 14,
          color,
        }}
      >
        {label}
      </Text>
    </Pressable>
  );
}

/**
 * Long-press action sheet for a message: quick reactions, reply-in-thread,
 * save, copy text / copy link (verified feedback, sheet stays open so the
 * outcome is visible where the tap happened), plus edit/delete for the
 * sender's own messages.
 */
export function MessageActionsSheet({
  target,
  isOwn,
  isSaved,
  onReact,
  onOpenPicker,
  onReplyInThread,
  onToggleSave,
  onCopyText,
  onCopyLink,
  onEdit,
  onDelete,
  onClose,
}: {
  target: Message;
  isOwn: boolean;
  isSaved: boolean;
  onReact: (emoji: string) => void;
  onOpenPicker: () => void;
  onReplyInThread: () => void;
  onToggleSave: () => void;
  onCopyText: () => Promise<boolean>;
  onCopyLink: () => Promise<boolean>;
  onEdit: () => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  const [textCopy, setTextCopy] = useState<CopyState>("idle");
  const [linkCopy, setLinkCopy] = useState<CopyState>("idle");
  const timersRef = useRef<ReturnType<typeof setTimeout>[]>([]);

  useEffect(() => {
    const timers = timersRef.current;
    return () => {
      for (const timer of timers) {
        clearTimeout(timer);
      }
    };
  }, []);

  const runCopy = (
    action: () => Promise<boolean>,
    set: (s: CopyState) => void,
  ) => {
    void action()
      .catch(() => false)
      .then((ok) => {
        set(ok ? "copied" : "failed");
        timersRef.current.push(
          setTimeout(() => set("idle"), COPY_FEEDBACK_MS),
        );
      });
  };

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

      <ActionButton
        testID="btn-reply-thread"
        icon={<Icon.thread color={semantic.ink} />}
        label="Reply in thread"
        onPress={onReplyInThread}
      />
      <ActionButton
        testID="btn-save"
        icon={
          <Icon.bookmark
            color={isSaved ? semantic.accent : semantic.ink}
          />
        }
        label={isSaved ? "Unsave message" : "Save message"}
        tone={isSaved ? "accent" : "default"}
        onPress={onToggleSave}
      />
      <ActionButton
        testID="btn-copy-text"
        icon={
          <Icon.copy
            color={
              textCopy === "copied"
                ? semantic.accent
                : textCopy === "failed"
                  ? semantic.danger
                  : semantic.ink
            }
          />
        }
        label={
          textCopy === "copied"
            ? "Copied"
            : textCopy === "failed"
              ? "Couldn't copy"
              : "Copy text"
        }
        tone={
          textCopy === "copied"
            ? "accent"
            : textCopy === "failed"
              ? "danger"
              : "default"
        }
        copyState={textCopy}
        onPress={() => runCopy(onCopyText, setTextCopy)}
      />
      <ActionButton
        testID="btn-copy-link"
        icon={
          <Icon.link
            color={
              linkCopy === "copied"
                ? semantic.accent
                : linkCopy === "failed"
                  ? semantic.danger
                  : semantic.ink
            }
          />
        }
        label={
          linkCopy === "copied"
            ? "Link copied"
            : linkCopy === "failed"
              ? "Couldn't copy link"
              : "Copy message link"
        }
        tone={
          linkCopy === "copied"
            ? "accent"
            : linkCopy === "failed"
              ? "danger"
              : "default"
        }
        copyState={linkCopy}
        onPress={() => runCopy(onCopyLink, setLinkCopy)}
      />

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
