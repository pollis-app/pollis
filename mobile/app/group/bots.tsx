import { useTranslation } from "react-i18next";
import { View } from "react-native";
import { observer } from "mobx-react-lite";
import {
  Screen,
  Header,
  Body,
  ListRow,
  Group,
  Button,
  BottomAction,
  EmptyState,
} from "../../components/ui";
import { Hint, ErrorText } from "../../components/groups/FormBits";
import { Icon } from "../../components/icons";
import { semantic, space } from "../../theme/tokens";
import { useGroupBots, useBot, useAutoApproveBotApps } from "../../hooks/queries";
import { useBotRoute } from "../../hooks/useBotRoute";
import { botCapReached } from "../../lib/bots";
import { useThemeRerender } from "../../components/theme";

/** Group settings → Bots (#1182): the group's bots, and "Add a bot". Admins only. */
function GroupBots() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const { router, gid: id, group, groupName, iAmAdmin } = useBotRoute();

  const { data: bots = [], isLoading, isError } = useGroupBots(iAmAdmin ? id : null);
  // The cap rides on `get_bot`; any one bot of the group answers it, and the list is cached.
  const { data: firstBot } = useBot(bots[0]?.userId ?? null);
  const atCap = botCapReached(bots.length, firstBot?.groupBotCap);
  // Waiting non-widening updates set to approve themselves land when an admin opens this (#2005).
  useAutoApproveBotApps(id, bots.map((b) => b.userId), iAmAdmin);

  return (
    <Screen testID="screen-group-bots" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack} title={t("bots.title")} subtitle={group?.name} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.lg }}>
        <Hint>{t("bots.intro", { group: groupName })}</Hint>
        {!iAmAdmin ? (
          <Hint>{t("bots.adminsOnly")}</Hint>
        ) : isLoading ? (
          <Hint>{t("common:states.loading")}</Hint>
        ) : isError ? (
          <ErrorText testID="text-bots-load-failed">{t("bots.loadFailed")}</ErrorText>
        ) : bots.length === 0 ? (
          <EmptyState testID="bots-empty">{t("bots.empty")}</EmptyState>
        ) : (
          <Group testID="list-bots">
            {bots.map((b) => (
              <ListRow
                key={b.userId}
                testID={`row-bot-${b.userId}`}
                glyph={<Icon.bot size={20} color={semantic.dim} />}
                name={b.displayName}
                sub={`@${b.username} · ${t("bots.hostCount", { n: b.hostCount })}`}
                chevron
                onPress={() =>
                  id
                    ? router.push({
                        pathname: "/group/bot",
                        params: { groupId: id, botId: b.userId },
                      })
                    : undefined
                }
              />
            ))}
          </Group>
        )}
        {iAmAdmin && atCap ? (
          <View testID="text-bot-cap-reached">
            <Hint>{t("bots.capReached", { cap: firstBot?.groupBotCap ?? bots.length })}</Hint>
          </View>
        ) : null}
      </Body>
      {iAmAdmin ? (
        <BottomAction>
          <Button
            full
            variant="primary"
            testID="btn-add-bot"
            icon={<Icon.plus size={16} />}
            disabled={!id || atCap}
            onPress={() =>
              id ? router.push({ pathname: "/group/add-bot", params: { groupId: id } }) : undefined
            }
          >
            {t("bots.add")}
          </Button>
        </BottomAction>
      ) : null}
    </Screen>
  );
}

export default observer(GroupBots);
