import { useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { Text } from "../../components/Text";
import { Screen, Header, Body, Group, ListRow } from "../../components/ui";
import { UserAvatar } from "../../components/UserAvatar";
import { useThemeRerender } from "../../components/theme";
import { semantic, type as ty, space } from "../../theme/tokens";
import { useUserGroupsWithChannels, useUserProfile } from "../../hooks/queries";
import { useMyAliases } from "../../hooks/queries/useGroupAliases";
import { appStore } from "../../stores/appStore";

/**
 * Self → Group aliases (#2003): every group the user is in, with what that group
 * shows (an alias, or the main profile). A row opens the group's alias editor.
 */
function GroupAliases() {
  useThemeRerender();
  const { t } = useTranslation("settings");
  const router = useRouter();
  const userId = appStore.currentUser?.id;
  const { data: groups = [] } = useUserGroupsWithChannels();
  const { data: aliases } = useMyAliases();
  const { data: profile } = useUserProfile();
  const mainName = profile?.preferred_name || profile?.username || "";

  return (
    <Screen testID="screen-self-aliases">
      <Header title={t("aliases.heading")} backTo={t("mobile:self.title")} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.lg }}>
        <Text style={[ty.secondary, { color: semantic.muted }]}>{t("aliases.intro")}</Text>
        {groups.length === 0 ? (
          <Text testID="text-aliases-empty" style={[ty.secondary, { color: semantic.muted }]}>
            {t("aliases.empty")}
          </Text>
        ) : (
          <Group>
            {groups.map((g) => {
              const alias = aliases?.get(g.id);
              return (
                <ListRow
                  key={g.id}
                  testID={`row-alias-${g.id}`}
                  minHeight={64}
                  glyph={
                    <UserAvatar
                      testID={`avatar-alias-${g.id}`}
                      userId={userId}
                      avatarKey={profile ? profile.avatar_url ?? null : undefined}
                      aliasKey={alias?.avatar_url}
                      label={alias?.display_name || mainName}
                      variant="self"
                      on="raised"
                    />
                  }
                  name={g.name}
                  sub={alias ? alias.display_name || mainName : t("aliases.mainProfile")}
                  chevron
                  onPress={() => router.push({ pathname: "/self/alias", params: { groupId: g.id } })}
                />
              );
            })}
          </Group>
        )}
      </Body>
    </Screen>
  );
}

export default observer(GroupAliases);
