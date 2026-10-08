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
        flexDirection: "row",
        alignItems: "center",
        flexWrap: "wrap",
        gap: space.xl,
        padding: space.xxl,
        borderRadius: r.xl,
        backgroundColor: semantic.panel,
      }}
    >
      <Avatar label={displayName} size="lg" variant="self" />
      <View
        accessible
        accessibilityLabel={`${displayName}, ${sub}`}
        style={{ flex: 1, minWidth: 120, gap: 2 }}
      >
        <Text accessibilityRole="header" numberOfLines={2} style={ty.title}>
          {displayName}
        </Text>
        <Text numberOfLines={2} style={ty.secondary}>
          {sub}
        </Text>
      </View>
      <Button testID="btn-edit-profile" onPress={onEdit}>
        {t("mobile:self.hub.editProfile")}
      </Button>
    </View>
  );
}
