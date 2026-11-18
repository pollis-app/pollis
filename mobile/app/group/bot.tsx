import { useState } from "react";
import { View } from "react-native";
import { useTranslation } from "react-i18next";
import { observer } from "mobx-react-lite";
import { useNav, useRouteParams } from "../../components/pane/paneContext";
import {
  Screen,
  Header,
  Body,
  SectionTitle,
  Group,
  ListRow,
  ActionRow,
  Chip,
  Card,
  EmptyState,
} from "../../components/ui";
import { Hint, ErrorText } from "../../components/groups/FormBits";
import { BotBadge } from "../../components/BotBadge";
import { Icon } from "../../components/icons";
import { semantic, type as ty, space } from "../../theme/tokens";
import { Text } from "../../components/Text";
import {
  useBot,
  useDeleteBot,
  useRevokeBotHost,
  useUserGroupsWithChannels,
} from "../../hooks/queries";
import { isLastHost } from "../../lib/bots";
import { formatRelative } from "../../lib/relativeTime";
import { haptic } from "../../lib/haptics";
import { useThemeRerender } from "../../components/theme";

/**
 * One bot (#1182): who manages it, its hosts (name, last seen, revoke, add), and delete. A host is a
 * device of the bot's user, so revoking one is an ordinary device revoke. With every host gone the
 * bot's identity cannot be recovered (by design), which the page says before it happens.
 */
function BotPage() {
  useThemeRerender();
  const { t } = useTranslation("mobile");
  const router = useNav();
  const { groupId, botId } = useRouteParams<{ groupId?: string; botId?: string }>();
  const gid = groupId ?? null;
  const bid = botId ?? null;

  const { data: groups = [] } = useUserGroupsWithChannels();
  const groupName = groups.find((g) => g.id === gid)?.name ?? t("group.common.fallbackName");
  const { data: bot, isLoading, isError } = useBot(bid);
  const revokeHost = useRevokeBotHost(bid, gid);
  const deleteBot = useDeleteBot(gid);

  const [confirmRevoke, setConfirmRevoke] = useState<string | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const hosts = bot?.hosts ?? [];
  const botName = bot?.displayName ?? t("bots.fallbackName");

  const onRevoke = (deviceId: string) => {
    if (confirmRevoke !== deviceId) {
      haptic("confirmArm");
      setConfirmRevoke(deviceId);
      return;
    }
    revokeHost.mutate(deviceId, {
      onSettled: () => setConfirmRevoke(null),
    });
  };

  const onDelete = () => {
    if (!confirmDelete) {
      haptic("confirmArm");
      setConfirmDelete(true);
      return;
    }
    if (!bid) {
      return;
    }
    deleteBot.mutate(bid, {
      onSuccess: () => router.back(),
    });
  };

  if (!bid || (!isLoading && !bot)) {
    return (
      <Screen testID="screen-bot" aboveTabBar={router.inPane}>
        <Header onBack={router.onBack} title={t("bots.title")} />
        <Body contentContainerStyle={{ padding: space.xxl }}>
          {isError ? (
            <ErrorText testID="text-bot-load-failed">{t("bots.loadFailed")}</ErrorText>
          ) : (
            <Hint>{t("bots.notFound")}</Hint>
          )}
        </Body>
      </Screen>
    );
  }

  return (
    <Screen testID="screen-bot" aboveTabBar={router.inPane}>
      <Header onBack={router.onBack} title={botName} subtitle={bot ? `@${bot.username}` : undefined} />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.lg }}>
        <Card style={{ gap: space.md }} testID="card-bot-about">
          <View style={{ flexDirection: "row", alignItems: "center", gap: space.sm }}>
            <BotBadge testID="badge-bot-page" />
            <Text style={[ty.secondary, { flex: 1 }]}>{t("bots.managedBy", { group: groupName })}</Text>
          </View>
          <Text style={ty.secondary}>{t("bots.machineNote", { group: groupName })}</Text>
        </Card>

        <SectionTitle style={{ paddingHorizontal: 0 }}>{t("bots.hostsSection")}</SectionTitle>
        <Hint>{t("bots.unrecoverable")}</Hint>
        {isLoading ? (
          <Hint>{t("common:states.loading")}</Hint>
        ) : hosts.length === 0 ? (
          <EmptyState testID="bot-no-hosts">{t("bots.noHosts")}</EmptyState>
        ) : (
          <Group testID="list-bot-hosts">
            {hosts.map((h) => {
              const armed = confirmRevoke === h.deviceId;
              const busy = revokeHost.isPending && revokeHost.variables === h.deviceId;
              const name = h.name.trim() || h.deviceId.slice(0, 8);
              return (
                <View key={h.deviceId}>
                  <ActionRow
                    testID={`row-bot-host-${h.deviceId}`}
                    glyph={<Icon.server size={20} color={semantic.dim} />}
                    name={name}
                    sub={
                      h.lastSeenAt
                        ? t("bots.lastSeen", { when: formatRelative(h.lastSeenAt) })
                        : t("bots.neverSeen")
                    }
                    action={
                      <Chip
                        variant="danger"
                        selected={armed}
                        loading={busy}
                        disabled={revokeHost.isPending && !busy}
                        testID={`btn-revoke-bot-host-${h.deviceId}`}
                        accessibilityLabel={
                          armed
                            ? t("group.settings.tapAgainToConfirm")
                            : t("bots.revokeLabel", { name })
                        }
                        onPress={() => onRevoke(h.deviceId)}
                      >
                        {armed ? t("group.common.confirm") : t("bots.revoke")}
                      </Chip>
                    }
                  />
                  {armed && isLastHost(hosts, h.deviceId) ? (
                    <View style={{ paddingHorizontal: space.xxl, paddingBottom: space.md }}>
                      <ErrorText testID="text-bot-last-host">{t("bots.lastHostWarning")}</ErrorText>
                    </View>
                  ) : null}
                </View>
              );
            })}
          </Group>
        )}
        {revokeHost.isError ? (
          <ErrorText>{(revokeHost.error as Error).message || t("bots.revokeFailed")}</ErrorText>
        ) : null}
        <Group>
          <ListRow
            testID="btn-add-bot-host"
            glyph={<Icon.plus size={20} color={semantic.dim} />}
            name={t("bots.addHostTitle")}
            sub={t("bots.addHostSub")}
            chevron
            onPress={() =>
              gid
                ? router.push({ pathname: "/group/add-bot", params: { groupId: gid, botId: bid } })
                : undefined
            }
          />
        </Group>

        {/* Delete: destructive, apart, two taps. */}
        <View style={{ paddingTop: space.xxxl * 2, gap: space.sm }}>
          <Group>
            <ListRow
              testID="btn-delete-bot"
              glyph={<Icon.trash size={20} color={semantic.danger} />}
              name={
                deleteBot.isPending
                  ? t("bots.deleting")
                  : confirmDelete
                    ? t("bots.deleteConfirm", { name: botName })
                    : t("bots.deleteBot")
              }
              nameStyle={{ color: semantic.danger }}
              sub={t("bots.deleteHint", { group: groupName })}
              disabled={deleteBot.isPending}
              onPress={onDelete}
            />
          </Group>
          {deleteBot.isError ? (
            <ErrorText>{(deleteBot.error as Error).message || t("bots.deleteFailed")}</ErrorText>
          ) : null}
        </View>
      </Body>
    </Screen>
  );
}

export default observer(BotPage);
