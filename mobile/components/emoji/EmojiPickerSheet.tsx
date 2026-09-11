import { useEffect, useMemo, useState } from "react";
import { View, Text, TextInput, Pressable, FlatList } from "react-native";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { semantic, type as ty, r, t as tint } from "../../theme/tokens";
import { SheetOverlay } from "../chat/SheetOverlay";
import { upper } from "../../i18n";
import {
  EMOJI_CATEGORIES,
  SKIN_TONES,
  STANDARD_EMOJI,
  applySkinTone,
  type EmojiCategoryId,
} from "./emojiData";
import {
  emojiDisplayChar,
  pickerEmojiId,
  pickerEmojiInsertText,
  resolveRecents,
  searchEmoji,
  type PickerEmoji,
} from "./emojiSearch";
import {
  hydrateEmojiPrefs,
  readRecentEmojiIds,
  readSkinTone,
  recordRecentEmojiId,
  writeSkinTone,
} from "../../lib/emojiPrefs";
import { useUsableEmoji, type CustomEmoji } from "../../hooks/queries/useEmoji";
import { CustomEmojiImage } from "./CustomEmojiImage";
import { emojiDisplayName, type EmojiAnnotationStack } from "./emojiAnnotations";
import { useEmojiAnnotations } from "./useEmojiAnnotations";

const COLUMNS = 8;

// Same base glyph as desktop's SkinTonePicker — a single string so the
// shaper joins the modifier.
const TONE_BASE = "\u{270B}";

type PickerListItem =
  | { type: "header"; key: string; label: string }
  | { type: "row"; key: string; items: PickerEmoji[] };

// One literal call per category so the catalogue check can see every key.
function categoryLabel(t: TFunction, id: EmojiCategoryId): string {
  switch (id) {
    case "people":
      return t("categories.people");
    case "nature":
      return t("categories.nature");
    case "food":
      return t("categories.food");
    case "activity":
      return t("categories.activity");
    case "travel":
      return t("categories.travel");
    case "objects":
      return t("categories.objects");
    case "symbols":
      return t("categories.symbols");
    case "flags":
      return t("categories.flags");
  }
}

function chunkRows(
  items: PickerEmoji[],
  keyPrefix: string,
): PickerListItem[] {
  const rows: PickerListItem[] = [];
  for (let i = 0; i < items.length; i += COLUMNS) {
    rows.push({
      type: "row",
      key: `${keyPrefix}-${i}`,
      items: items.slice(i, i + COLUMNS),
    });
  }
  return rows;
}

function Cell({
  item,
  toneIndex,
  annotations,
  onPick,
}: {
  item: PickerEmoji;
  toneIndex: number;
  annotations: EmojiAnnotationStack;
  onPick: (item: PickerEmoji) => void;
}) {
  return (
    <Pressable
      onPress={() => onPick(item)}
      accessibilityRole="button"
      accessibilityLabel={
        item.kind === "standard"
          ? emojiDisplayName(item.emoji, annotations)
          : `:${item.emoji.shortcode}:`
      }
      style={{
        flex: 1,
        aspectRatio: 1,
        alignItems: "center",
        justifyContent: "center",
      }}
    >
      {item.kind === "standard" ? (
        <Text style={{ fontSize: 24 }}>
          {emojiDisplayChar(item.emoji, item.emoji.tonable ? toneIndex : 0)}
        </Text>
      ) : (
        <CustomEmojiImage
          shortcode={item.emoji.shortcode}
          contentHash={item.emoji.content_hash}
          size={26}
        />
      )}
    </Pressable>
  );
}

/**
 * Full emoji picker (search, categories, skin tones, custom group emoji) in
 * the long-press sheet pattern. `onSelect` receives the insert text: the
 * displayed Unicode character, or a `<:name:hash>` token for custom emoji.
 */
export function EmojiPickerSheet({
  onSelect,
  onClose,
}: {
  onSelect: (text: string) => void;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation("emoji");
  const annotations = useEmojiAnnotations(i18n.language);
  const [query, setQuery] = useState("");
  const [toneIndex, setToneIndex] = useState(readSkinTone);
  const [recentIds, setRecentIds] = useState<string[]>(readRecentEmojiIds);
  const { data: customEmoji = [] } = useUsableEmoji();

  // Prefs hydrate from disk once; refresh local state when that lands.
  useEffect(() => {
    let cancelled = false;
    void hydrateEmojiPrefs().then(() => {
      if (!cancelled) {
        setToneIndex(readSkinTone());
        setRecentIds(readRecentEmojiIds());
      }
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const items = useMemo<PickerListItem[]>(() => {
    const needle = query.trim();
    if (needle) {
      return chunkRows(searchEmoji(needle, customEmoji, annotations), "search");
    }
    const out: PickerListItem[] = [];
    const recents = resolveRecents(recentIds, customEmoji);
    if (recents.length > 0) {
      out.push({
        type: "header",
        key: "h-recents",
        label: upper(t("picker.recent")),
      });
      out.push(...chunkRows(recents.slice(0, COLUMNS * 2), "recents"));
    }
    // Custom emoji, one section per owning group.
    const byGroup = new Map<string, CustomEmoji[]>();
    for (const e of customEmoji) {
      const list = byGroup.get(e.group_id) ?? [];
      list.push(e);
      byGroup.set(e.group_id, list);
    }
    for (const [groupId, list] of byGroup) {
      out.push({
        type: "header",
        key: `h-${groupId}`,
        label: upper(list[0]?.group_name || t("mobile:emoji.groupFallback")),
      });
      out.push(
        ...chunkRows(
          list.map((emoji) => ({ kind: "custom" as const, emoji })),
          `g-${groupId}`,
        ),
      );
    }
    for (const category of EMOJI_CATEGORIES) {
      out.push({
        type: "header",
        key: `h-${category.id}`,
        label: upper(categoryLabel(t, category.id as EmojiCategoryId)),
      });
      const inCategory = STANDARD_EMOJI.filter(
        (e) => e.category === (category.id as EmojiCategoryId),
      ).map((emoji) => ({ kind: "standard" as const, emoji }));
      out.push(...chunkRows(inCategory, category.id));
    }
    return out;
  }, [query, customEmoji, recentIds, annotations, t]);

  const onPick = (item: PickerEmoji) => {
    const text = pickerEmojiInsertText(item, toneIndex);
    setRecentIds(recordRecentEmojiId(pickerEmojiId(item)));
    onSelect(text);
  };

  const onTone = (index: number) => {
    setToneIndex(index);
    writeSkinTone(index);
  };

  return (
    <SheetOverlay onClose={onClose}>
      <View style={{ height: 420, gap: 10 }}>
        <TextInput
          testID="input-emoji-search"
          accessibilityLabel={t("picker.searchPlaceholder")}
          value={query}
          onChangeText={setQuery}
          placeholder={t("picker.searchPlaceholder")}
          placeholderTextColor={semantic.mute}
          autoCorrect={false}
          autoCapitalize="none"
          style={{
            borderWidth: 1,
            borderColor: semantic.hairStrong,
            borderRadius: r.sm,
            paddingVertical: 8,
            paddingHorizontal: 12,
            fontFamily: ty.body.fontFamily,
            fontSize: 14,
            color: semantic.ink,
            backgroundColor: semantic.fieldBg,
          }}
        />
        <View style={{ flexDirection: "row", gap: 6 }}>
          {SKIN_TONES.map((tone, index) => (
            <Pressable
              key={index}
              testID={`btn-tone-${index}`}
              accessibilityRole="radio"
              accessibilityState={{ selected: toneIndex === index }}
              accessibilityLabel={
                index === 0
                  ? t("skinTone.default")
                  : t("skinTone.numbered", { index })
              }
              onPress={() => onTone(index)}
              style={{
                width: 34,
                height: 34,
                alignItems: "center",
                justifyContent: "center",
                borderWidth: 1,
                borderColor:
                  toneIndex === index ? semantic.accent : semantic.hair,
                borderRadius: r.sm,
                backgroundColor:
                  toneIndex === index ? tint(0.12) : "transparent",
              }}
            >
              <Text style={{ fontSize: 18 }}>{`${TONE_BASE}${tone}`}</Text>
            </Pressable>
          ))}
        </View>
        <FlatList
          testID="list-emoji"
          data={items}
          keyExtractor={(item) => item.key}
          initialNumToRender={16}
          windowSize={7}
          keyboardShouldPersistTaps="handled"
          renderItem={({ item }) => {
            if (item.type === "header") {
              return (
                <Text
                  style={[
                    ty.label,
                    { letterSpacing: 1.8, paddingTop: 12, paddingBottom: 4 },
                  ]}
                >
                  {item.label}
                </Text>
              );
            }
            return (
              <View style={{ flexDirection: "row" }}>
                {item.items.map((cell) => (
                  <Cell
                    key={pickerEmojiId(cell)}
                    item={cell}
                    toneIndex={toneIndex}
                    annotations={annotations}
                    onPick={onPick}
                  />
                ))}
                {item.items.length < COLUMNS
                  ? Array.from({ length: COLUMNS - item.items.length }).map(
                      (_, i) => <View key={`pad-${i}`} style={{ flex: 1 }} />,
                    )
                  : null}
              </View>
            );
          }}
          ListEmptyComponent={
            <Text
              style={{
                fontFamily: ty.body.fontFamily,
                fontSize: 13,
                color: semantic.mute,
                paddingTop: 16,
              }}
            >
              {t("picker.empty")}
            </Text>
          }
        />
      </View>
    </SheetOverlay>
  );
}
