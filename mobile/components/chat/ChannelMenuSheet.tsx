import { useTranslation } from "react-i18next";
import { Icon } from "../icons";
import { Group, ListRow } from "../ui";
import { ToggleRow } from "../self/ToggleRow";
import { semantic } from "../../theme/tokens";
import { useMutedChannels, useSetChannelMuted } from "../../hooks/queries";
import { SheetOverlay } from "./SheetOverlay";

/** "More" menu for a channel: mute (#1431), details, settings. */
export function ChannelMenuSheet({
  title,
  channelId,
  onInfo,
  onGroupSettings,
  onClose,
}: {
  title: string;
  channelId: string;
  onInfo: () => void;
  onGroupSettings?: () => void;
  onClose: () => void;
}) {
  const { t } = useTranslation("mobile");
  const { data: muted } = useMutedChannels();
  const setMuted = useSetChannelMuted();
  const isMuted = muted?.has(channelId) ?? false;
  return (
    <SheetOverlay title={title} onClose={onClose} closeTestID="btn-menu-cancel">
      <Group surface="high">
        {/* Account-wide mute; a direct @mention still pushes. Optimistic. */}
        <ToggleRow
          testID="toggle-channel-mute"
          label={t("channels:channel.muteLabel")}
          sub={t("channels:channel.muteDescription")}
          on={isMuted}
          disabled={setMuted.isPending}
          onToggle={() => setMuted.mutate({ channelId, muted: !isMuted })}
        />
      </Group>
      {/* Touch-down (#1529); the mute switch stays on release. */}
      <Group surface="high">
        <ListRow
          testID="btn-menu-info"
          pressIn
          glyph={<Icon.info size={22} color={semantic.text} />}
          name={t("nav:panel.ariaLabel")}
          chevron
          onPress={onInfo}
        />
        {onGroupSettings ? (
          <ListRow
            testID="btn-menu-group-settings"
            pressIn
            glyph={<Icon.gear size={22} color={semantic.text} />}
            name={t("chat.groupSettings")}
            chevron
            onPress={onGroupSettings}
          />
        ) : null}
      </Group>
    </SheetOverlay>
  );
}
