import { Text, View } from "react-native";
import { useTranslation } from "react-i18next";
import { Avatar, Button } from "../ui";
import { semantic, type as ty, r, space } from "../../theme/tokens";

/** Top of the Self tab (You.dc.html): avatar, name, handle, Edit profile. */
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
        gap: space.lg,
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
      </View>
      {/* Below the name rather than beside it, so it never squeezes the text. */}
      <View style={{ alignItems: "flex-start" }}>
        <Button testID="btn-edit-profile" onPress={onEdit}>
          {t("mobile:self.hub.editProfile")}
        </Button>
      </View>
    </View>
  );
}
