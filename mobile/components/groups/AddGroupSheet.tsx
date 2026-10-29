import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { SheetOverlay, afterSheetClose } from "../chat/SheetOverlay";
import { Group, ListRow } from "../ui";
import { Icon } from "../icons";
import { semantic } from "../../theme/tokens";

export function AddGroupSheet({ onClose }: { onClose: () => void }) {
  const { t } = useTranslation("mobile");
  const router = useRouter();
  return (
    <SheetOverlay title={t("groups.addGroup")} onClose={onClose} testID="sheet-add-group">
      {/* Touch-down (#1529). */}
      <Group surface="high">
        <ListRow
          testID="btn-create-group"
          pressIn
          glyph={<Icon.plus size={22} color={semantic.dim} />}
          name={t("groups.create")}
          sub={t("groups.createSub")}
          chevron
          onPress={() => {
            onClose();
            afterSheetClose(() => router.push("/group/new"));
          }}
        />
        <ListRow
          testID="btn-join-group"
          pressIn
          glyph={<Icon.search size={22} color={semantic.dim} />}
          name={t("groups.find")}
          sub={t("groups.findSub")}
          chevron
          onPress={() => {
            onClose();
            afterSheetClose(() => router.push("/group/discover"));
          }}
        />
      </Group>
    </SheetOverlay>
  );
}
