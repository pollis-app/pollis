import { Text } from "react-native";
import { useTranslation } from "react-i18next";
import { Screen, Header, Body, Group, Avatar, Chip, ActionRow } from "../../components/ui";
import { ErrorText } from "../../components/self/SettingsField";
import { type as ty, space } from "../../theme/tokens";
import { activeLocale } from "../../i18n";
import { useBlockedUsers, useUnblockUser } from "../../hooks/queries";

export default function Blocked() {
  const { t } = useTranslation("dms");
  const { data: blocked = [], isLoading } = useBlockedUsers();
  const unblock = useUnblockUser();

  return (
    <Screen testID="screen-self-blocked" centered>
      <Header
        title={t("mobile:self.security.blockedUsers")}
        subtitle={isLoading ? undefined : String(blocked.length)}
        backTo={t("settings:security.title")}
      />
      <Body contentContainerStyle={{ paddingHorizontal: space.xxl, paddingTop: space.xxl, gap: space.lg }}>
        {isLoading ? <Text style={ty.secondary}>{t("common:states.loading")}</Text> : null}
        {!isLoading && blocked.length === 0 ? (
          <Text style={ty.secondary}>{t("blocked.empty")}</Text>
        ) : null}
        {blocked.length > 0 ? (
          <Group>
            {blocked.map((b) => {
              const handle = b.username ?? b.user_id.slice(0, 8);
              const sub = t("mobile:self.blocked.blockedOn", {
                date: new Date(b.blocked_at).toLocaleDateString(activeLocale()),
              });
              return (
                <ActionRow
                  key={b.user_id}
                  testID={`row-blocked-${b.user_id}`}
                  glyph={<Avatar label={handle} />}
                  name={`@${handle}`}
                  sub={sub}
                  action={
                    <Chip
                      variant="outline"
                      testID={`btn-unblock-${b.user_id}`}
                      accessibilityLabel={`${t("mobile:self.blocked.unblock")} @${handle}`}
                      disabled={unblock.isPending}
                      onPress={() => unblock.mutate(b.user_id)}
                    >
                      {t("mobile:self.blocked.unblock")}
                    </Chip>
                  }
                />
              );
            })}
          </Group>
        ) : null}
        {unblock.isError ? (
          <ErrorText>
            {(unblock.error as Error).message || t("mobile:self.blocked.unblockFailed")}
          </ErrorText>
        ) : null}
      </Body>
    </Screen>
  );
}
