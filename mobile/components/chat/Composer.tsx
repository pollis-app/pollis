import { useEffect, useMemo, useRef, useState } from "react";
import { View, Text, Pressable, Platform, ScrollView, type TextInput } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Avatar, Field, PressInPressable } from "../ui";
import { fonts, layout, semantic, type as ty, r } from "../../theme/tokens";
import {
  applyMention,
  mentionQueryAt,
  rankMentionCandidates,
  type MentionCandidate,
} from "../../lib/mentions";
import {
  applyShortcode,
  completedShortcodeAt,
  customShortcodeEntries,
  rankShortcodeEntries,
  resolveShortcode,
  shortcodeQueryAt,
  standardShortcodeEntries,
  type ShortcodeEntry,
} from "../../lib/emojiShortcodes";
import { CustomEmojiImage } from "../emoji/CustomEmojiImage";
import { EmojiPickerSheet } from "../emoji/EmojiPickerSheet";
import type { CustomEmoji } from "../../hooks/queries/useEmoji";
import type { PickedAttachment } from "../../lib/attachments";
import { ComposerAttachmentPreview } from "./ComposerAttachmentPreview";
import { useOpenDraftMediaViewer } from "../../hooks/useOpenMediaViewer";

/** Bottom composer bar. `@…` and `:…` open suggestions; shortcodes are substituted HERE, before send, so the wire format is unchanged. */
export function Composer({
  draft,
  onChangeDraft,
  onSend,
  sendPending,
  editable,
  mentionCandidates,
  customEmoji,
  onAttach,
  pendingAttachments,
  onRemoveAttachment,
  onChangeAttachment,
  canSendEmptyText = false,
  focusRequest = 0,
  placeholder,
}: {
  draft: string;
  onChangeDraft: (text: string) => void;
  onSend: () => void;
  sendPending: boolean;
  editable: boolean;
  mentionCandidates?: MentionCandidate[];
  customEmoji?: CustomEmoji[];
  onAttach?: () => void;
  pendingAttachments?: PickedAttachment[];
  onRemoveAttachment?: (id: string) => void;
  /** Marks a pending attachment sensitive, or records its cover's blurhash (#1568). */
  onChangeAttachment?: (id: string, patch: Partial<PickedAttachment>) => void;
  canSendEmptyText?: boolean;
  focusRequest?: number;
  placeholder?: string;
}) {
  const { t } = useTranslation("common");
  const openDraft = useOpenDraftMediaViewer();
  const [caret, setCaret] = useState(0);
  // iOS keeps a multiline field's grown height when `value` resets: cap an empty field at its measured one-line height.
  const [emptyHeight, setEmptyHeight] = useState<number | null>(null);
  const [pickerOpen, setPickerOpen] = useState(false);
  const inputRef = useRef<TextInput>(null);
  useEffect(() => {
    if (focusRequest > 0) {
      inputRef.current?.focus();
    }
  }, [focusRequest]);

  const mentionQuery =
    mentionCandidates && mentionCandidates.length > 0
      ? mentionQueryAt(draft, Math.min(caret, draft.length))
      : null;
  const suggestions = mentionQuery
    ? rankMentionCandidates(mentionCandidates ?? [], mentionQuery.query)
    : [];

  const acceptMention = (candidate: MentionCandidate) => {
    const next = applyMention(draft, Math.min(caret, draft.length), candidate.username);
    if (next.text === draft) {
      return;
    }
    onChangeDraft(next.text);
    // RN moves the native caret to the end after a programmatic change.
    setCaret(next.caret);
  };

  // ~1600 rows: built once per mount, not per keystroke.
  const shortcodeEntries = useMemo(
    () => [...customShortcodeEntries(customEmoji ?? []), ...standardShortcodeEntries()],
    [customEmoji],
  );

  const emojiQuery = mentionQuery
    ? null
    : shortcodeQueryAt(draft, Math.min(caret, draft.length));
  const emojiSuggestions = emojiQuery
    ? rankShortcodeEntries(shortcodeEntries, emojiQuery.query)
    : [];

  const acceptEmoji = (entry: ShortcodeEntry) => {
    if (!emojiQuery) {
      return;
    }
    const next = applyShortcode(
      draft,
      emojiQuery.start,
      emojiQuery.end,
      entry.insertText,
      true,
    );
    onChangeDraft(next.text);
    setCaret(next.caret);
  };

  // The ':' closing a known shortcode is swallowed for the emoji (single-character insertions at the end only).
  const handleChangeText = (next: string) => {
    if (next.length === draft.length + 1 && next.endsWith(":")) {
      const closed = completedShortcodeAt(next, next.length);
      const entry = closed ? resolveShortcode(shortcodeEntries, closed.name) : undefined;
      if (closed && entry) {
        const applied = applyShortcode(
          next,
          closed.start,
          closed.end,
          entry.insertText,
          false,
        );
        onChangeDraft(applied.text);
        setCaret(applied.caret);
        return;
      }
    }
    onChangeDraft(next);
  };

  const insertEmoji = (insert: string) => {
    const at = Math.min(caret, draft.length);
    onChangeDraft(draft.slice(0, at) + insert + draft.slice(at));
    setCaret(at + insert.length);
    setPickerOpen(false);
  };

  const sendDisabled = (!draft.trim() && !canSendEmptyText) || sendPending;
  const hint = placeholder ?? t("composer.placeholder");

  return (
    <View>
      {suggestions.length > 0 ? (
        <View
          testID="list-mention-suggestions"
          style={suggestionBox()}
        >
          <ScrollView keyboardShouldPersistTaps="always">
            {suggestions.map((candidate, i) => (
              <Pressable
                key={candidate.userId}
                testID={`row-mention-${candidate.username}`}
                accessibilityRole="button"
                accessibilityLabel={t("mobile:chat.mentionLabel", {
                  name: candidate.username,
                })}
                onPress={() => acceptMention(candidate)}
                style={({ pressed }) => suggestionRow(pressed, i)}
              >
                <Avatar label={candidate.username} size="sm" />
                <Text style={[ty.body, { color: semantic.text }]}>
                  @{candidate.username}
                </Text>
              </Pressable>
            ))}
          </ScrollView>
        </View>
      ) : null}
      {emojiSuggestions.length > 0 ? (
        <View
          testID="list-emoji-suggestions"
          style={suggestionBox()}
        >
          <ScrollView keyboardShouldPersistTaps="always">
            {emojiSuggestions.map((entry, i) => (
              <Pressable
                key={`${entry.custom ? "c" : "s"}:${entry.shortcode}`}
                testID={`row-emoji-${entry.shortcode}`}
                accessibilityRole="button"
                accessibilityLabel={t("mobile:chat.emojiSuggestionLabel", {
                  shortcode: entry.shortcode,
                })}
                onPress={() => acceptEmoji(entry)}
                style={({ pressed }) => suggestionRow(pressed, i)}
              >
                {entry.custom && entry.contentHash ? (
                  <CustomEmojiImage
                    shortcode={entry.shortcode}
                    contentHash={entry.contentHash}
                    size={22}
                  />
                ) : (
                  <Text style={{ fontSize: 20 }}>{entry.char}</Text>
                )}
                <Text style={[ty.body, { color: semantic.text }]}>
                  :{entry.shortcode}:
                </Text>
                <Text
                  numberOfLines={1}
                  style={[ty.meta, { flexShrink: 1 }]}
                >
                  {entry.label}
                </Text>
              </Pressable>
            ))}
          </ScrollView>
        </View>
      ) : null}
      {pendingAttachments && pendingAttachments.length > 0 ? (
        <ScrollView
          testID="strip-attachments"
          horizontal
          showsHorizontalScrollIndicator={false}
          keyboardShouldPersistTaps="handled"
          style={{
            flexGrow: 0,
            borderTopWidth: 1,
            borderTopColor: semantic.hairSoft,
          }}
          contentContainerStyle={{
            gap: 8,
            paddingHorizontal: 12,
            paddingTop: 8,
            paddingBottom: 2,
          }}
        >
          {pendingAttachments.map((att, index) => (
            <ComposerAttachmentPreview
              key={att.id}
              attachment={att}
              index={index}
              onRemove={(id) => onRemoveAttachment?.(id)}
              onChange={onChangeAttachment}
              onOpen={(picked) => openDraft(pendingAttachments, picked.id)}
            />
          ))}
        </ScrollView>
      ) : null}
      <View
        style={{
          flexDirection: "row",
          alignItems: "flex-end",
          gap: 8,
          paddingTop: 8,
          paddingBottom: 8,
          paddingStart: 8,
          paddingEnd: 12,
          borderTopWidth: pendingAttachments && pendingAttachments.length > 0 ? 0 : 1,
          borderTopColor: semantic.hairSoft,
          backgroundColor: semantic.bg,
        }}
      >
        {/* Touch-down (#1529). Send stays on release. */}
        <PressInPressable
          pressIn
          testID="btn-attach"
          accessibilityRole="button"
          accessibilityLabel={t("composer.addAttachment")}
          onPress={onAttach}
          disabled={!onAttach}
          accessibilityState={{ disabled: !onAttach }}
          style={({ pressed }) => ({
            width: layout.touchMin,
            height: layout.touchMin,
            borderRadius: layout.touchMin / 2,
            alignItems: "center",
            justifyContent: "center",
            backgroundColor: pressed && onAttach ? semantic.high : semantic.raised,
          })}
        >
          <Icon.plus size={22} color={onAttach ? semantic.accent : semantic.dim} />
        </PressInPressable>
        <View style={{ flex: 1, minWidth: 0 }}>
          <Field
            ref={inputRef}
            testID="input-composer"
            accessibilityLabel={t("composer.inputLabel")}
            // The visible hint is an overlay screen readers skip.
            accessibilityHint={hint}
            value={draft}
            onChangeText={handleChangeText}
            onSelectionChange={(e) => setCaret(e.nativeEvent.selection.end)}
            // Return inserts a newline (#1247); only the send button sends.
            multiline
            autoCapitalize="sentences"
            editable={editable}
            containerStyle={{
              alignItems: "flex-end",
              borderRadius: layout.touchMin / 2,
              paddingVertical: 0,
              paddingStart: FIELD_PAD_START,
              paddingEnd: FIELD_PAD_END,
              gap: FIELD_GAP,
            }}
            onLayout={(e) => {
              if (emptyHeight === null && draft === "") {
                setEmptyHeight(e.nativeEvent.layout.height);
              }
            }}
            style={{
              maxHeight:
                Platform.OS === "ios" && draft === "" && emptyHeight !== null
                  ? emptyHeight
                  : layout.composerInputMax,
              paddingTop: 11,
              paddingBottom: 11,
              textAlignVertical: "center",
            }}
            trailing={
              <PressInPressable
                pressIn
                testID="btn-composer-emoji"
                accessibilityRole="button"
                accessibilityLabel={t("emoji:picker.triggerLabel")}
                onPress={() => setPickerOpen(true)}
                disabled={!editable}
                hitSlop={4}
                style={{
                  width: EMOJI_BUTTON_WIDTH,
                  height: 42,
                  alignItems: "center",
                  justifyContent: "center",
                }}
              >
                <Icon.smile size={22} color={semantic.dim} />
              </PressInPressable>
            }
          />
          {draft === "" ? (
            // Drawn here so it is always ONE line (a multiline input sizes to its placeholder).
            <View
              pointerEvents="none"
              accessibilityElementsHidden
              importantForAccessibility="no-hide-descendants"
              style={{
                position: "absolute",
                top: 0,
                bottom: 0,
                start: FIELD_PAD_START,
                end: FIELD_PAD_END + FIELD_GAP + EMOJI_BUTTON_WIDTH,
                justifyContent: "center",
              }}
            >
              <Text
                testID="composer-placeholder"
                numberOfLines={1}
                ellipsizeMode="tail"
                style={{
                  fontFamily: fonts.regular,
                  fontSize: 16,
                  color: semantic.muted,
                }}
              >
                {hint}
              </Text>
            </View>
          ) : null}
        </View>
        {/* With nothing to send it is announced disabled and leaves the focus order. */}
        <Pressable
          onPress={onSend}
          disabled={sendDisabled}
          focusable={!sendDisabled}
          testID="btn-send"
          accessibilityRole="button"
          accessibilityLabel={t("composer.send")}
          accessibilityState={{ disabled: sendDisabled }}
          style={{
            width: layout.touchMin,
            height: layout.touchMin,
            borderRadius: layout.touchMin / 2,
            alignItems: "center",
            justifyContent: "center",
            backgroundColor: semantic.accent,
          }}
        >
          <Icon.arrowUp size={22} color={semantic.onAccent} />
        </Pressable>
      </View>
      {pickerOpen ? (
        <EmojiPickerSheet
          title={t("emoji:picker.triggerLabel")}
          onSelect={insertEmoji}
          onClose={() => setPickerOpen(false)}
        />
      ) : null}
    </View>
  );
}

const FIELD_PAD_START = 16;
const FIELD_PAD_END = 4;
const FIELD_GAP = 4;
const EMOJI_BUTTON_WIDTH = 36;

function suggestionBox() {
  return {
    marginHorizontal: 12,
    marginBottom: 6,
    borderRadius: r.lg,
    backgroundColor: semantic.raised,
    maxHeight: 240,
    overflow: "hidden" as const,
  };
}

function suggestionRow(pressed: boolean, index: number) {
  return {
    flexDirection: "row" as const,
    alignItems: "center" as const,
    gap: 12,
    minHeight: layout.touchMin,
    paddingHorizontal: 14,
    paddingVertical: 6,
    borderTopWidth: index > 0 ? 1 : 0,
    borderTopColor: semantic.hairSoft,
    backgroundColor: pressed ? semantic.high : "transparent",
  };
}
