import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Button, Group, ListRow } from "../ui";
import { semantic } from "../../theme/tokens";
import { SheetOverlay } from "./SheetOverlay";

/**
 * "More" menu for a channel conversation: conversation details + group
 * settings, as grouped 52pt rows in the standard sheet. `title` is the
 * channel's name (the sheet's spoken name).
 */
export function ChannelMenuSheet({
  title,
  onInfo,
  onGroupSettings,
  onClose,
}: {
  title: string;
  onInfo: () => void;
  onGroupSettings?: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation("mobile");
  return (
    <SheetOverlay title={title} onClose={onClose}>
      <Group surface="high">
        <ListRow
          testID="btn-menu-info"
          glyph={<Icon.info size={22} color={semantic.text} />}
          name={t("nav:panel.ariaLabel")}
          chevron
          onPress={onInfo}
        />
        {onGroupSettings ? (
          <ListRow
            testID="btn-menu-group-settings"
            glyph={<Icon.gear size={22} color={semantic.text} />}
            name={t("chat.groupSettings")}
            chevron
            onPress={onGroupSettings}
          />
        ) : null}
      </Group>
      <Button full variant="subtle" testID="btn-menu-cancel" onPress={onClose}>
        {t("common:actions.cancel")}
      </Button>
    </SheetOverlay>
  );
}
