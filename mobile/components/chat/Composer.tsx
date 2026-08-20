import { useMemo, useState } from "react";
import { View, Text, TextInput, Pressable, ScrollView } from "react-native";
import { Icon } from "../icons";
import { Avatar } from "../ui";
import { semantic, type as ty, r } from "../../theme/tokens";
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
import type { CustomEmoji } from "../../hooks/queries/useEmoji";
import type { PickedAttachment } from "../../lib/attachments";

/**
 * Bottom composer bar: attach button, text input, send button. When
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
  const [caret, setCaret] = useState(0);

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

  return (
    <View>
      {suggestions.length > 0 ? (
        <View
          testID="list-mention-suggestions"
          style={{
            marginHorizontal: 12,
            marginBottom: 4,
            borderWidth: 1,
            borderColor: semantic.hairStrong,
            borderRadius: r.sm,
            backgroundColor: semantic.cardBg,
            maxHeight: 220,
          }}
        >
          <ScrollView keyboardShouldPersistTaps="always">
            {suggestions.map((candidate) => (
              <Pressable
                key={candidate.userId}
                testID={`row-mention-${candidate.username}`}
                accessibilityRole="button"
                accessibilityLabel={`Mention ${candidate.username}`}
                onPress={() => acceptMention(candidate)}
                style={{
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 10,
                  paddingHorizontal: 12,
                  paddingVertical: 9,
                }}
              >
                <Avatar label={candidate.username.slice(0, 2)} />
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 14,
                    color: semantic.ink,
                  }}
                >
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
          style={{
            marginHorizontal: 12,
            marginBottom: 4,
            borderWidth: 1,
            borderColor: semantic.hairStrong,
            borderRadius: r.sm,
            backgroundColor: semantic.cardBg,
            maxHeight: 220,
          }}
        >
          <ScrollView keyboardShouldPersistTaps="always">
            {emojiSuggestions.map((entry) => (
              <Pressable
                key={`${entry.custom ? "c" : "s"}:${entry.shortcode}`}
                testID={`row-emoji-${entry.shortcode}`}
                accessibilityRole="button"
                accessibilityLabel={`Emoji :${entry.shortcode}:`}
                onPress={() => acceptEmoji(entry)}
                style={{
                  flexDirection: "row",
                  alignItems: "center",
                  gap: 10,
                  paddingHorizontal: 12,
                  paddingVertical: 9,
                }}
              >
                {entry.custom && entry.contentHash ? (
                  <CustomEmojiImage
                    shortcode={entry.shortcode}
                    contentHash={entry.contentHash}
                    size={20}
                  />
                ) : (
                  <Text style={{ fontSize: 18 }}>{entry.char}</Text>
                )}
                <Text
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 14,
                    color: semantic.ink,
                  }}
                >
                  :{entry.shortcode}:
                </Text>
                <Text
                  numberOfLines={1}
                  style={{
                    fontFamily: ty.body.fontFamily,
                    fontSize: 12,
                    color: semantic.mute,
                    flexShrink: 1,
                  }}
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
            gap: 6,
            paddingHorizontal: 12,
            paddingBottom: 4,
          }}
        >
          {pendingAttachments.map((att) => (
            <Pressable
              key={att.id}
              testID={`chip-attachment-${att.id}`}
              accessibilityRole="button"
              accessibilityLabel={`Remove attachment ${att.name}`}
              onPress={() => onRemoveAttachment?.(att.id)}
              style={{
                flexDirection: "row",
                alignItems: "center",
                gap: 6,
                paddingHorizontal: 8,
                paddingVertical: 5,
                borderWidth: 1,
                borderColor: semantic.hairStrong,
                borderRadius: r.sm,
              }}
            >
              <Text
                numberOfLines={1}
                style={{
                  fontFamily: ty.body.fontFamily,
                  fontSize: 12,
                  color: semantic.ink,
                  maxWidth: 160,
                }}
              >
                {att.name}
              </Text>
              <Icon.exit color={semantic.mute} size={12} />
            </Pressable>
          ))}
        </View>
      ) : null}
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
          onPress={onAttach}
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
          onChangeText={handleChangeText}
          onSelectionChange={(e) => setCaret(e.nativeEvent.selection.end)}
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
          disabled={(!draft.trim() && !canSendEmptyText) || sendPending}
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
            opacity:
              (!draft.trim() && !canSendEmptyText) || sendPending ? 0.4 : 1,
          }}
        >
          <Icon.send color="#0a0907" />
        </Pressable>
      </View>
    </View>
  );
}
