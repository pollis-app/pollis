import { useEffect, useRef, useState } from "react";
import { View, Text, Pressable, ScrollView, useWindowDimensions } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Button, Group, ListRow } from "../ui";
import { fonts, layout, semantic, type as ty, r } from "../../theme/tokens";
import { SheetOverlay } from "./SheetOverlay";
import { MessageBodyInline } from "./MessageBody";
import type { Message } from "../../hooks/queries";

const QUICK_EMOJI = ["👍", "❤️", "😂", "🎉", "🔥", "🙏"];

// #897: copy feedback is verified — the state comes from the clipboard
// call's boolean, never assumed. Matches desktop's 2s reset.
type CopyState = "idle" | "copied" | "failed";
const COPY_FEEDBACK_MS = 2000;

const GLYPH = 22;

// Copied / failed rows say so in their label; the accent is a second cue.
function copyColor(state: CopyState) {
  return state === "idle" ? semantic.text : semantic.accent;
}

/**
 * Long-press action sheet for a message (Actions.dc.html): the message
 * quoted in a plain card, quick reactions, then grouped 52pt rows — reply in
 * thread, add reaction, copy text / copy link (verified feedback; the sheet
 * stays open so the outcome shows where the tap happened), save — and a
 * separate group for edit/delete on the sender's own messages or report on
 * anyone else's.
 */
export function MessageActionsSheet({
  target,
  quoteName,
  quoteTime,
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
  onReport,
  onClose,
}: {
  target: Message;
  // Display name and time for the quoted message.
  quoteName: string;
  quoteTime: string;
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
  /** Report the sender (#1213). Shown only on other people's messages. */
  onReport?: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation("chat");
  const { height } = useWindowDimensions();
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

  const textCopyLabel =
    textCopy === "copied"
      ? t("mobile:chat.copied")
      : textCopy === "failed"
        ? t("mobile:chat.copyFailed")
        : t("mobile:chat.copyText");
  const linkCopyLabel =
    linkCopy === "copied"
      ? t("actions.copyLinkCopied")
      : linkCopy === "failed"
        ? t("actions.copyLinkFailed")
        : t("actions.copyLink");

  return (
    <SheetOverlay title={t("mobile:chat.messageSheetTitle")} onClose={onClose}>
      {/* Scrolls when large text makes the sheet taller than the screen. */}
      <ScrollView
        style={{ maxHeight: height * 0.72 }}
        contentContainerStyle={{ gap: 12 }}
        bounces={false}
      >
        {/* The message being acted on: a plain card, no edge stripe. */}
        <View
          accessible
          style={{
            paddingVertical: 10,
            paddingHorizontal: 12,
            borderRadius: r.md,
            borderWidth: 1,
            borderColor: semantic.hair,
            backgroundColor: semantic.panel,
            gap: 2,
          }}
        >
          <Text style={[ty.section, { fontFamily: fonts.bold, color: semantic.dim }]}>
            {`${quoteName} · ${quoteTime}`}
          </Text>
          {target.content ? (
            <Text
              numberOfLines={3}
              style={{
                fontFamily: fonts.regular,
                fontSize: 15,
                lineHeight: 21,
                color: semantic.text,
              }}
            >
              <MessageBodyInline text={target.content} emojiSize={16} />
            </Text>
          ) : null}
        </View>

        <View
          style={{
            flexDirection: "row",
            justifyContent: "space-between",
            gap: 6,
          }}
        >
          {QUICK_EMOJI.map((emoji, ei) => (
            <Pressable
              key={emoji}
              testID={`btn-react-${ei}`}
              accessibilityRole="button"
              accessibilityLabel={t("mobile:chat.reactWith", { emoji })}
              onPress={() => onReact(emoji)}
              style={({ pressed }) => ({
                width: layout.touchMin + 4,
                height: layout.touchMin + 4,
                borderRadius: (layout.touchMin + 4) / 2,
                alignItems: "center",
                justifyContent: "center",
                backgroundColor: pressed ? semantic.accentSoft : semantic.high,
              })}
            >
              <Text style={{ fontSize: 24 }}>{emoji}</Text>
            </Pressable>
          ))}
        </View>

        <Group surface="high">
          <ListRow
            testID="btn-reply-thread"
            glyph={<Icon.thread size={GLYPH} color={semantic.text} />}
            name={t("actions.replyInThread")}
            onPress={onReplyInThread}
          />
          <ListRow
            testID="btn-react-more"
            glyph={<Icon.smilePlus size={GLYPH} color={semantic.text} />}
            name={t("reactions.add")}
            onPress={onOpenPicker}
          />
          <ListRow
            testID="btn-copy-text"
            glyph={<Icon.copy size={GLYPH} color={copyColor(textCopy)} />}
            name={textCopyLabel}
            nameStyle={{ color: copyColor(textCopy) }}
            onPress={() => runCopy(onCopyText, setTextCopy)}
          />
          <ListRow
            testID="btn-copy-link"
            glyph={<Icon.link size={GLYPH} color={copyColor(linkCopy)} />}
            name={linkCopyLabel}
            nameStyle={{ color: copyColor(linkCopy) }}
            onPress={() => runCopy(onCopyLink, setLinkCopy)}
          />
          <ListRow
            testID="btn-save"
            glyph={
              <Icon.bookmark
                size={GLYPH}
                color={isSaved ? semantic.accent : semantic.text}
              />
            }
            name={isSaved ? t("actions.removeBookmark") : t("actions.save")}
            onPress={onToggleSave}
          />
        </Group>

        {isOwn ? (
          <Group surface="high">
            <ListRow
              testID="btn-edit"
              glyph={<Icon.pencil size={GLYPH} color={semantic.text} />}
              name={t("actions.edit")}
              onPress={onEdit}
            />
            {/* Destructive = label + trash icon + its own group, no third hue. */}
            <ListRow
              testID="btn-delete"
              glyph={<Icon.trash size={GLYPH} color={semantic.danger} />}
              name={t("actions.delete")}
              nameStyle={{ fontFamily: fonts.semibold, color: semantic.danger }}
              onPress={onDelete}
            />
          </Group>
        ) : null}

        {!isOwn && onReport ? (
          <Group surface="high">
            <ListRow
              testID="btn-report"
              glyph={<Icon.flag size={GLYPH} color={semantic.danger} />}
              name={t("actions.report")}
              nameStyle={{ fontFamily: fonts.semibold, color: semantic.danger }}
              onPress={onReport}
            />
          </Group>
        ) : null}

        <Button
          full
          variant="subtle"
          testID="btn-action-cancel"
          onPress={onClose}
        >
          {t("common:actions.cancel")}
        </Button>
      </ScrollView>
    </SheetOverlay>
  );
}
