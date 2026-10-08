import { useEffect, useMemo, useState } from "react";
import { View, Text, Pressable, FlatList, useWindowDimensions } from "react-native";
import { useTranslation } from "react-i18next";
import type { TFunction } from "i18next";
import { layout, semantic, type as ty } from "../../theme/tokens";
import { Field } from "../ui";
import { Icon } from "../icons";
import { SheetOverlay } from "../chat/SheetOverlay";
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

// Cells are at least this wide (≥44pt targets); the column count follows the
// sheet width, so a narrow phone gets 7 and a wide one more.
const MIN_CELL = 46;
// The sheet's horizontal padding (SheetOverlay: 16 each side).
const SHEET_INSET = 32;

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
  columns: number,
): PickerListItem[] {
  const rows: PickerListItem[] = [];
  for (let i = 0; i < items.length; i += columns) {
    rows.push({
      type: "row",
      key: `${keyPrefix}-${i}`,
      items: items.slice(i, i + columns),
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
      style={({ pressed }) => ({
        flex: 1,
        aspectRatio: 1,
        minHeight: layout.touchMin,
        alignItems: "center",
        justifyContent: "center",
        borderRadius: 12,
        backgroundColor: pressed ? semantic.high : "transparent",
      })}
    >
      {item.kind === "standard" ? (
        <Text style={{ fontSize: 26 }}>
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
 * the standard sheet. `title` names the sheet (default "Add reaction"). `onSelect` receives the insert text: the
 * displayed Unicode character, or a `<:name:hash>` token for custom emoji.
 */
export function EmojiPickerSheet({
  title,
  onSelect,
  onClose,
}: {
  title?: string;
  onSelect: (text: string) => void;
  onClose: () => void;
}) {
  const { t, i18n } = useTranslation("emoji");
  const { width } = useWindowDimensions();
  const columns = Math.max(6, Math.min(12, Math.floor((width - SHEET_INSET) / MIN_CELL)));
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
      return chunkRows(searchEmoji(needle, customEmoji, annotations), "search", columns);
    }
    const out: PickerListItem[] = [];
    const recents = resolveRecents(recentIds, customEmoji);
    if (recents.length > 0) {
      out.push({
        type: "header",
        key: "h-recents",
        label: t("picker.recent"),
      });
      out.push(...chunkRows(recents.slice(0, columns * 2), "recents", columns));
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
        label: list[0]?.group_name || t("mobile:emoji.groupFallback"),
      });
      out.push(
        ...chunkRows(
          list.map((emoji) => ({ kind: "custom" as const, emoji })),
          `g-${groupId}`,
          columns,
        ),
      );
    }
    for (const category of EMOJI_CATEGORIES) {
      out.push({
        type: "header",
        key: `h-${category.id}`,
        label: categoryLabel(t, category.id as EmojiCategoryId),
      });
      const inCategory = STANDARD_EMOJI.filter(
        (e) => e.category === (category.id as EmojiCategoryId),
      ).map((emoji) => ({ kind: "standard" as const, emoji }));
      out.push(...chunkRows(inCategory, category.id, columns));
    }
    return out;
  }, [query, customEmoji, recentIds, annotations, t, columns]);

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
    <SheetOverlay title={title ?? t("chat:reactions.add")} onClose={onClose}>
      <View style={{ height: 460, gap: 12 }}>
        <Field
          testID="input-emoji-search"
          accessibilityLabel={t("picker.searchPlaceholder")}
          value={query}
          onChangeText={setQuery}
          placeholder={t("picker.searchPlaceholder")}
          autoCorrect={false}
          autoCapitalize="none"
          icon={<Icon.search size={20} color={semantic.muted} />}
        />
        <View style={{ flexDirection: "row", flexWrap: "wrap", gap: 6 }}>
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
                width: layout.touchMin,
                height: layout.touchMin,
                alignItems: "center",
                justifyContent: "center",
                borderWidth: toneIndex === index ? 2 : 1,
                borderColor:
                  toneIndex === index ? semantic.accentLine : semantic.hair,
                borderRadius: layout.touchMin / 2,
                backgroundColor:
                  toneIndex === index ? semantic.accentSoft : "transparent",
              }}
            >
              <Text style={{ fontSize: 20 }}>{`${TONE_BASE}${tone}`}</Text>
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
                  accessibilityRole="header"
                  style={[ty.section, { paddingTop: 14, paddingBottom: 6 }]}
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
                {item.items.length < columns
                  ? Array.from({ length: columns - item.items.length }).map(
                      (_, i) => <View key={`pad-${i}`} style={{ flex: 1 }} />,
                    )
                  : null}
              </View>
            );
          }}
          ListEmptyComponent={
            <Text style={[ty.secondary, { color: semantic.muted, paddingTop: 16 }]}>
              {t("picker.empty")}
            </Text>
          }
        />
      </View>
    </SheetOverlay>
  );
}
