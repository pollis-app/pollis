import { useMemo } from "react";
import { Pressable, View } from "react-native";
import { Text } from "../Text";
import { KeyboardController } from "react-native-keyboard-controller";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { MessageBodyInline } from "../chat/MessageBody";
import { MessageAttachments } from "../chat/MessageAttachments";
import { haptic } from "../../lib/haptics";
import { linkA11yActions, runLinkA11yAction } from "../../lib/openLink";
import { LONG_PRESS_MS } from "../../lib/pressFeedback";
import { semantic, type as ty, fonts, space } from "../../theme/tokens";
import type { MessageAttachment } from "../../types";

/** One Vault entry (#1316); a screen reader gets the sheet's actions on the row. */
export function VaultEntryRow({
  entryId,
  time,
  text,
  attachments,
  pinned,
  edited,
  sendState,
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
  /** Still in the Vault's queue (#2100). A pending save looks saved, as a sending message does. */
  sendState?: "pending" | "failed" | "rejected";
  onOpenActions: () => void;
  onTogglePin: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onOpenAttachment: (attachment: MessageAttachment) => void;
}) {
  const { t } = useTranslation("vault");

  // The entry sheet opens with the keyboard going down, as the chat's message sheet (#1859).
  const onLongPressTouch = () => {
    haptic("longPress");
    void KeyboardController.dismiss();
    onOpenActions();
  };

  const notSaved = sendState === "failed" || sendState === "rejected";
  const queued = sendState !== undefined;
  const status = [
    notSaved ? t("entry.notSaved") : sendState === "pending" ? t("entry.queued") : time,
    pinned ? t("entry.pinned") : null,
    edited ? t("entry.edited") : null,
  ]
    .filter(Boolean)
    .join(", ");
  const label = text
    ? `${text}, ${status}`
    : `${attachments.map((a) => a.filename).join(", ")}, ${status}`;

  const linkActions = useMemo(
    () => linkA11yActions(text, (host) => t("mobile:link.openLabel", { host })),
    [text, t],
  );
  const a11yActions = [
    { name: "activate" },
    { name: "longpress" },
    ...(queued
      ? []
      : [
          { name: "togglePin", label: pinned ? t("entry.unpin") : t("entry.pin") },
          { name: "edit", label: t("entry.edit") },
          { name: "delete", label: t("entry.delete") },
        ]),
    ...attachments.map((a, i) => ({
      name: `openAttachment${i}`,
      label: t("chat:attachment.openLabel", { filename: a.filename }),
    })),
    ...linkActions.map(({ name, label }) => ({ name, label })),
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
        if (runLinkA11yAction(action, linkActions)) {
          return;
        }
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
        {notSaved ? (
          <Text
            testID={`vault-not-saved-${entryId}`}
            style={[ty.meta, { fontFamily: fonts.semibold, color: semantic.danger }]}
          >
            {t("entry.notSaved")}
          </Text>
        ) : null}
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
        <MessageAttachments
          attachments={attachments}
          onOpen={onOpenAttachment}
          onLongPress={onLongPressTouch}
        />
      ) : null}
    </Pressable>
  );
}
