import { Pressable, Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { MessageBodyInline } from "../chat/MessageBody";
import { AttachmentTile } from "../chat/AttachmentTile";
import { haptic } from "../../lib/haptics";
import { LONG_PRESS_MS } from "../../lib/pressFeedback";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";
import type { MessageAttachment } from "../../types";

/**
 * One Vault entry (#107). Desktop's hover actions live in the sheet a long
 * press opens; a screen reader gets them as actions on the row.
 */
export function VaultEntryRow({
  entryId,
  time,
  text,
  attachments,
  pinned,
  edited,
  onOpenActions,
  onTogglePin,
  onEdit,
  onDelete,
  onOpenAttachment,
}: {
  entryId: string;
  time: string;
  text: string;
  attachments: MessageAttachment[];
  pinned: boolean;
  edited: boolean;
  onOpenActions: () => void;
  onTogglePin: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onOpenAttachment: (attachment: MessageAttachment) => void;
}) {
  const { t } = useTranslation("vault");

  const onLongPressTouch = () => {
    haptic("longPress");
    onOpenActions();
  };

  const status = [time, pinned ? t("entry.pinned") : null, edited ? t("entry.edited") : null]
    .filter(Boolean)
    .join(", ");
  const label = text
    ? `${text}, ${status}`
    : `${attachments.map((a) => a.filename).join(", ")}, ${status}`;

  const a11yActions = [
    { name: "activate" },
    { name: "longpress" },
    { name: "togglePin", label: pinned ? t("entry.unpin") : t("entry.pin") },
    { name: "edit", label: t("entry.edit") },
    { name: "delete", label: t("entry.delete") },
    ...attachments.map((a, i) => ({
      name: `openAttachment${i}`,
      label: t("chat:attachment.openLabel", { filename: a.filename }),
    })),
  ];

  return (
    <Pressable
      testID={`row-vault-${entryId}`}
      onLongPress={onLongPressTouch}
      delayLongPress={LONG_PRESS_MS}
      accessible
      accessibilityLabel={label}
      accessibilityActions={a11yActions}
      onAccessibilityAction={(e) => {
        const action = e.nativeEvent.actionName;
        if (action === "togglePin") {
          onTogglePin();
        } else if (action === "edit") {
          onEdit();
        } else if (action === "delete") {
          onDelete();
        } else if (action.startsWith("openAttachment")) {
          const target = attachments[Number(action.slice("openAttachment".length))];
          if (target) {
            onOpenAttachment(target);
          }
        } else if (action === "activate" || action === "longpress") {
          onOpenActions();
        }
      }}
      style={({ pressed }) => ({
        paddingHorizontal: space.lg,
        paddingVertical: space.sm,
        gap: 4,
        backgroundColor: pressed ? semantic.raised : semantic.bg,
      })}
    >
      <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
        <Text style={ty.meta}>{time}</Text>
        {pinned ? (
          <View
            testID={`vault-pinned-${entryId}`}
            style={{ flexDirection: "row", alignItems: "center", gap: 4 }}
          >
            <Icon.pin size={12} color={semantic.accent} />
            <Text style={[ty.meta, { fontFamily: fonts.semibold, color: semantic.accent }]}>
              {t("entry.pinned")}
            </Text>
          </View>
        ) : null}
        {edited ? <Text style={ty.meta}>{t("entry.edited")}</Text> : null}
      </View>
      {text ? (
        <Text testID={`text-vault-${entryId}`} style={[ty.body, { color: semantic.text }]}>
          <MessageBodyInline text={text} />
        </Text>
      ) : null}
      {attachments.length > 0 ? (
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6, paddingTop: 2 }}>
          {attachments.map((attachment) => (
            <AttachmentTile
              key={attachment.id}
              attachment={attachment}
              onOpen={onOpenAttachment}
              onLongPress={onLongPressTouch}
            />
          ))}
        </View>
      ) : null}
    </Pressable>
  );
}
