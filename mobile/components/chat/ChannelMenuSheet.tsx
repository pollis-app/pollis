import { Text, Pressable } from "react-native";
import { Icon } from "../icons";
import { semantic, type as ty, r } from "../../theme/tokens";
import { SheetOverlay } from "./SheetOverlay";

function MenuItem({
  icon,
  label,
  onPress,
  testID,
}: {
  icon: React.ReactNode;
  label: string;
  onPress: () => void;
  testID?: string;
}) {
  return (
    <Pressable
      onPress={onPress}
      testID={testID}
      accessibilityRole="button"
      accessibilityLabel={label}
      style={{
        paddingVertical: 14,
        paddingHorizontal: 12,
        borderWidth: 1,
        borderColor: semantic.hairStrong,
        borderRadius: r.sm,
        flexDirection: "row",
        alignItems: "center",
        gap: 10,
      }}
    >
      {icon}
      <Text
        style={{
          fontFamily: ty.body.fontFamily,
          fontSize: 14,
          color: semantic.ink,
        }}
      >
        {label}
      </Text>
    </Pressable>
  );
}

/**
 * Kebab menu for a channel conversation: conversation info + group settings.
 */
export function ChannelMenuSheet({
  onInfo,
  onGroupSettings,
  onClose,
}: {
  onInfo: () => void;
  onGroupSettings?: () => void;
  onClose: () => void;
}) {
  return (
    <SheetOverlay onClose={onClose}>
      <MenuItem
        testID="btn-menu-info"
        icon={<Icon.info color={semantic.ink} />}
        label="Conversation info"
        onPress={onInfo}
      />
      {onGroupSettings ? (
        <MenuItem
          testID="btn-menu-group-settings"
          icon={<Icon.gear color={semantic.ink} />}
          label="Group settings"
          onPress={onGroupSettings}
        />
      ) : null}
      <Pressable
        onPress={onClose}
        testID="btn-menu-cancel"
        accessibilityRole="button"
        accessibilityLabel="Cancel"
        style={{
          paddingVertical: 14,
          alignItems: "center",
        }}
      >
        <Text style={[ty.label, { color: semantic.mute }]}>CANCEL</Text>
      </Pressable>
    </SheetOverlay>
  );
}
