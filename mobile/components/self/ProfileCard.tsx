import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Avatar, IconButton } from "../ui";
import { Icon } from "../icons";
import { semantic, type as ty, r, space } from "../../theme/tokens";

/** Top of the Self tab (You.dc.html): avatar, name, handle, and an Edit profile icon button. */
export function ProfileCard({
  displayName,
  handle,
  onEdit,
}: {
  displayName: string;
  handle: string;
  onEdit: () => void;
}) {
  const { t } = useTranslation("mobile");
  const sub = t("mobile:self.hub.profileSub", { handle });
  return (
    <View
      testID="card-self-profile"
      style={{
        padding: space.xxl,
        borderRadius: r.xl,
        backgroundColor: semantic.panel,
      }}
    >
      <View style={{ flexDirection: "row", alignItems: "center", gap: space.xl }}>
        <Avatar label={displayName} size="lg" variant="self" />
        <View
          accessible
          accessibilityLabel={`${displayName}, ${sub}`}
          style={{ flex: 1, minWidth: 0, gap: 2 }}
        >
          {/* One line each, cut at the end — long generated names and
              handles must not wrap and then truncate mid-word. */}
          <Text
            accessibilityRole="header"
            numberOfLines={1}
            ellipsizeMode="tail"
            style={ty.title}
          >
            {displayName}
          </Text>
          <Text numberOfLines={1} ellipsizeMode="tail" style={ty.secondary}>
            {sub}
          </Text>
        </View>
        {/* Icon only (user's call); the spoken label keeps the full name. */}
        <IconButton
          testID="btn-edit-profile"
          filled
          accessibilityLabel={t("mobile:self.hub.editProfile")}
          onPress={onEdit}
          icon={<Icon.pencil size={20} color={semantic.text} />}
        />
      </View>
    </View>
  );
}
