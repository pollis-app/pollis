import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Group, ListRow } from "../ui";
import { SheetOverlay } from "../chat/SheetOverlay";
import { MessageBodyInline } from "../chat/MessageBody";
import { fonts, semantic, type as ty, r } from "../../theme/tokens";

const GLYPH = 22;

/**
 * Long-press sheet for a Vault entry: the entry quoted in a plain card, then
 * desktop's three row actions — pin/unpin and edit, and delete in its own
 * group (destructive = label + trash icon + separation, no third hue).
 * Delete does not act here: it hands over to the delete bar, which asks first.
 */
export function VaultEntrySheet({
  quote,
  quoteTime,
  pinned,
  onTogglePin,
  onEdit,
  onDelete,
  onClose,
}: {
  // The entry's caption, or its filenames when it has none.
  quote: string;
  quoteTime: string;
  pinned: boolean;
  onTogglePin: () => void;
  onEdit: () => void;
  onDelete: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation("vault");
  return (
    <SheetOverlay
      title={t("mobile:vault.sheetTitle")}
      onClose={onClose}
      testID="sheet-vault-entry"
    >
      <View style={{ gap: 12 }}>
        <View
          accessible
          style={{
            paddingVertical: 10,
            paddingHorizontal: 12,
            borderRadius: r.md,
            backgroundColor: semantic.panel,
            gap: 2,
          }}
        >
          <Text style={[ty.section, { fontFamily: fonts.bold, color: semantic.dim }]}>
            {quoteTime}
          </Text>
          {quote ? (
            <Text numberOfLines={3} style={[ty.body, { color: semantic.text }]}>
              <MessageBodyInline text={quote} emojiSize={16} />
            </Text>
          ) : null}
        </View>

        <Group surface="high">
          <ListRow
            testID="btn-vault-pin"
            glyph={
              pinned ? (
                <Icon.pinOff size={GLYPH} color={semantic.text} />
              ) : (
                <Icon.pin size={GLYPH} color={semantic.text} />
              )
            }
            name={pinned ? t("entry.unpin") : t("entry.pin")}
            onPress={onTogglePin}
          />
          <ListRow
            testID="btn-vault-edit"
            glyph={<Icon.pencil size={GLYPH} color={semantic.text} />}
            name={t("entry.edit")}
            onPress={onEdit}
          />
        </Group>

        <Group surface="high">
          <ListRow
            testID="btn-vault-delete"
            glyph={<Icon.trash size={GLYPH} color={semantic.danger} />}
            name={t("entry.delete")}
            nameStyle={{ fontFamily: fonts.semibold, color: semantic.danger }}
            onPress={onDelete}
          />
        </Group>
      </View>
    </SheetOverlay>
  );
}
