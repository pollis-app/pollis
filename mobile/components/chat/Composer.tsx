import { useMemo, useState } from "react";
import { View, Text, Pressable, ScrollView } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Avatar, Field } from "../ui";
import { layout, semantic, type as ty, r } from "../../theme/tokens";
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

/**
 * Bottom composer bar (Chat.dc.html): a 44pt round attach button, a raised
 * pill field that grows with its text (emoji button inside at the end), and
 * a 44pt round accent send button. When
 * `mentionCandidates` is provided, typing `@…` opens a suggestion list
 * above the input (#886) — candidates come from the visible roster only,
 * ranked like desktop (prefix beats substring, alphabetical within rank).
 *
 * Typing `:…` opens the same list for emoji, and typing the closing `:` of a
 * complete shortcode substitutes it outright, Slack-style. Both tracks mirror
 * desktop: a trigger only opens at a word start (so `http://` and `10:30` are
 * inert), custom emoji beat standard ones, and everything is substituted HERE,
 * before send — a standard emoji becomes the literal character, a custom one
 * the existing `<:shortcode:hash>` token, so the wire format is unchanged.
 *
 * PRECEDENCE: an open mention query suppresses the emoji one. The two cannot
 * in fact both be open — neither body alphabet contains the other's trigger —
 * but the order is fixed so a future widening has one place to be reasoned
 * about.
 */
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
  canSendEmptyText = false,
}: {
  draft: string;
  onChangeDraft: (text: string) => void;
  onSend: () => void;
  sendPending: boolean;
  editable: boolean;
  mentionCandidates?: MentionCandidate[];
  /** Every custom emoji the user may send. Omitted, only standard ones complete. */
  customEmoji?: CustomEmoji[];
  onAttach?: () => void;
  pendingAttachments?: PickedAttachment[];
  onRemoveAttachment?: (id: string) => void;
  /** True when attachments alone make the message sendable. */
  canSendEmptyText?: boolean;
}) {
  const { t } = useTranslation("common");
  const [caret, setCaret] = useState(0);
  const [pickerOpen, setPickerOpen] = useState(false);

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
    // RN moves the native caret to the end after a programmatic value
    // change; track the logical caret so the query closes either way.
    setCaret(next.caret);
  };

  // The standard half is ~1600 rows off a table this app already bundles for
  // the picker, so it is built once per mount rather than per keystroke.
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

  // Slack's direct substitution, on the way through: the ':' that CLOSES a
  // known shortcode is swallowed and the emoji takes its place. Gated on a
  // single-character insertion so a paste ending in a colon is left alone.
  // RN's `onChangeText` carries no caret, so this only fires for a colon typed
  // at the END of the draft — the overwhelmingly common case, and editing back
  // into the middle of a line still has the suggestion list.
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

  // The in-field emoji button inserts the picked emoji (or custom token) at
  // the caret.
  const insertEmoji = (insert: string) => {
    const at = Math.min(caret, draft.length);
    onChangeDraft(draft.slice(0, at) + insert + draft.slice(at));
    setCaret(at + insert.length);
    setPickerOpen(false);
  };

  const sendDisabled = (!draft.trim() && !canSendEmptyText) || sendPending;

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
        <View
          testID="strip-attachments"
          style={{
            flexDirection: "row",
            flexWrap: "wrap",
            gap: 8,
            paddingHorizontal: 12,
            paddingTop: 8,
            paddingBottom: 2,
            borderTopWidth: 1,
            borderTopColor: semantic.hairSoft,
          }}
        >
          {pendingAttachments.map((att) => (
            <Pressable
              key={att.id}
              testID={`chip-attachment-${att.id}`}
              accessibilityRole="button"
              accessibilityLabel={t("composer.removeAttachment", {
                name: att.name,
              })}
              onPress={() => onRemoveAttachment?.(att.id)}
              // 32pt chip; the slop brings the target to 44pt.
              hitSlop={6}
              style={{
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
                minHeight: 32,
                paddingStart: 12,
                paddingEnd: 8,
                borderWidth: 1,
                borderColor: semantic.edge,
                borderRadius: r.pill,
                backgroundColor: semantic.raised,
              }}
            >
              <Text
                numberOfLines={1}
                style={[ty.secondary, { color: semantic.text, maxWidth: 160 }]}
              >
                {att.name}
              </Text>
              <Icon.close color={semantic.muted} size={16} />
            </Pressable>
          ))}
        </View>
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
        <Pressable
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
            // Unavailable: dashed edge ring + dim glyph, never a fade.
            backgroundColor: pressed && onAttach ? semantic.high : semantic.raised,
            borderWidth: onAttach ? 0 : 1,
            borderColor: semantic.edge,
            borderStyle: "dashed",
          })}
        >
          <Icon.plus size={22} color={onAttach ? semantic.text : semantic.dim} />
        </Pressable>
        <Field
          testID="input-composer"
          accessibilityLabel={t("composer.inputLabel")}
          value={draft}
          onChangeText={handleChangeText}
          onSelectionChange={(e) => setCaret(e.nativeEvent.selection.end)}
          placeholder={t("composer.placeholder")}
          onSubmitEditing={onSend}
          returnKeyType="send"
          // Multiline so long drafts wrap and the field grows; Return still
          // sends (the submit behaviour the single-line field had).
          multiline
          submitBehavior="submit"
          autoCapitalize="sentences"
          editable={editable}
          containerStyle={{
            flex: 1,
            minWidth: 0,
            alignItems: "flex-end",
            borderRadius: layout.touchMin / 2,
            paddingVertical: 0,
            paddingStart: 16,
            paddingEnd: 4,
            gap: 4,
          }}
          style={{
            maxHeight: 140,
            paddingTop: 11,
            paddingBottom: 11,
            textAlignVertical: "center",
          }}
          trailing={
            <Pressable
              testID="btn-composer-emoji"
              accessibilityRole="button"
              accessibilityLabel={t("emoji:picker.triggerLabel")}
              onPress={() => setPickerOpen(true)}
              disabled={!editable}
              hitSlop={4}
              style={{
                width: 36,
                height: 42,
                alignItems: "center",
                justifyContent: "center",
              }}
            >
              <Icon.smile size={22} color={semantic.dim} />
            </Pressable>
          }
        />
        {/* Disabled send is told apart by more than colour: neutral fill +
            edge ring + muted glyph, and it leaves the focus order. */}
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
            backgroundColor: sendDisabled ? semantic.raised : semantic.accent,
            borderWidth: sendDisabled ? 1 : 0,
            borderColor: semantic.edge,
          }}
        >
          <Icon.arrowUp
            size={22}
            color={sendDisabled ? semantic.muted : semantic.onAccent}
          />
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

// The suggestion popover above the composer (mentions / emoji shortcodes).
function suggestionBox() {
  return {
    marginHorizontal: 12,
    marginBottom: 6,
    borderWidth: 1,
    borderColor: semantic.hair,
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
