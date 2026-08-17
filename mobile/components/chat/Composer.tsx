import { useState } from "react";
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

/**
 * Bottom composer bar: attach button, text input, send button. When
 * `mentionCandidates` is provided, typing `@…` opens a suggestion list
 * above the input (#886) — candidates come from the visible roster only,
 * ranked like desktop (prefix beats substring, alphabetical within rank).
 */
export function Composer({
  draft,
  onChangeDraft,
  onSend,
  sendPending,
  editable,
  mentionCandidates,
}: {
  draft: string;
  onChangeDraft: (text: string) => void;
  onSend: () => void;
  sendPending: boolean;
  editable: boolean;
  mentionCandidates?: MentionCandidate[];
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
    </View>
  );
}
