import { View, Text } from "react-native";
import { useTranslation } from "react-i18next";
import {
  Screen,
  Crumb,
  Body,
  SectionTitle,
  ListRow,
  Avatar,
  Chip,
  Ctx,
} from "../../components/ui";
import { semantic, type as ty } from "../../theme/tokens";
import { activeLocale, upper } from "../../i18n";
import { useBlockedUsers, useUnblockUser } from "../../hooks/queries";

export default function Blocked() {
  const { t } = useTranslation("dms");
  const { data: blocked = [], isLoading } = useBlockedUsers();
  const unblock = useUnblockUser();

  return (
    <Screen testID="screen-self-blocked" centered>
      <Crumb
        segs={[
          { label: upper(t("mobile:self.title")) },
          { label: t("mobile:self.blocked.title"), leaf: true },
        ]}
        end={String(blocked.length)}
      />
      <Body>
        <SectionTitle>{upper(t("blocked.pageTitle"))}</SectionTitle>
        {isLoading ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("common:states.loading")}
          </Text>
        ) : null}
        {!isLoading && blocked.length === 0 ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 13,
              color: semantic.mute,
              paddingHorizontal: 18,
              paddingVertical: 12,
            }}
          >
            {t("blocked.empty")}
          </Text>
        ) : null}
        {blocked.map((b) => {
          const handle = b.blocked_username ?? b.blocked_id.slice(0, 8);
          return (
            <ListRow
              key={b.blocked_id}
              testID={`row-blocked-${b.blocked_id}`}
              minHeight={54}
              glyph={<Avatar label={handle.slice(0, 2)} />}
              name={`@${handle}`}
              nameStyle={{ fontSize: 14 }}
              sub={t("mobile:self.blocked.blockedOn", {
                date: new Date(b.created_at).toLocaleDateString(activeLocale()),
              })}
              end={
                <Chip
                  testID={`btn-unblock-${b.blocked_id}`}
                  accessibilityLabel={t("mobile:self.blocked.unblock")}
                  onPress={() => unblock.mutate(b.blocked_id)}
                >
                  {unblock.isPending ? "…" : t("mobile:self.blocked.unblock")}
                </Chip>
              }
            />
          );
        })}
        {unblock.isError ? (
          <Text
            style={{
              fontFamily: ty.body.fontFamily,
              fontSize: 12,
              color: semantic.danger,
              paddingHorizontal: 18,
              paddingTop: 6,
            }}
          >
            {(unblock.error as Error).message ||
              t("mobile:self.blocked.unblockFailed")}
          </Text>
        ) : null}
      </Body>
      <Ctx
        cr={upper(t("mobile:self.title"))}
        name={t("mobile:self.blocked.title")}
      />
    </Screen>
  );
}
