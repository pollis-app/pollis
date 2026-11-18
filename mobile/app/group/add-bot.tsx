import { useState } from "react";
import { View } from "react-native";
import { observer } from "mobx-react-lite";
import { Text } from "../../components/Text";
import { Screen, Header, Body, Field, Button, BottomAction, Card } from "../../components/ui";
import { LabeledField, Hint, ErrorText } from "../../components/groups/FormBits";
import { SasCodeField } from "../../components/SasCodeField";
import { Icon } from "../../components/icons";
import { semantic, type as ty, r, space } from "../../theme/tokens";
import { useAddBotHost, useBot, useCreateBot } from "../../hooks/queries";
import { useBotRoute } from "../../hooks/useBotRoute";
import { useSubmitLock } from "../../hooks/useSubmitLock";
import { useCopyFeedback } from "../../hooks/useCopyFeedback";
import { SAS_LENGTH } from "../../lib/enrollmentSas";
import { BOT_PAIR_COMMAND, botErrorKey } from "../../lib/bots";
import { useThemeRerender } from "../../components/theme";

/**
 * "Add a bot" and "Add a host" (#1182): run `pollis-bot pair` on the machine, then TYPE the code it
 * prints. The code is never pre-filled (#1096): the core checks it against the one it derives from
 * the pairing request's key, so a swapped request fails here. With `botId` this adds a host to that
 * bot; without, it creates a bot in `groupId`.
 */
function AddBot() {
  useThemeRerender();
  const { t, router, gid, bid, groupName } = useBotRoute();
  const addingHost = bid !== null;

  const { data: bot } = useBot(bid);
  const createBot = useCreateBot(gid);
  const addHost = useAddBotHost(bid, gid);
  const submit = useSubmitLock();
  const { copyState, copy } = useCopyFeedback();

  const [name, setName] = useState("");
  const [code, setCode] = useState("");
  const [error, setError] = useState<string | null>(null);

  const ready = code.length === SAS_LENGTH && (addingHost || name.trim().length > 0);

  const fail = (e: unknown, fallback: string) => {
    submit.end();
    const message = (e as Error)?.message ?? "";
    const key = botErrorKey(message);
    // A wrong or stale code means reading the machine's screen again.
    if (key) {
      setCode("");
    }
    setError(key ? t(key) : message || fallback);
  };

  const onSubmit = () => {
    if (!ready || !submit.begin()) {
      return;
    }
    setError(null);
    if (addingHost) {
      addHost.mutate(code, {
        onSuccess: () => router.back(),
        onError: (e) => fail(e, t("bots.errors.hostFailed")),
      });
      return;
    }
    createBot.mutate(
      { code, displayName: name.trim() },
      {
        onSuccess: (created) =>
          gid
            ? router.replace({
                pathname: "/group/bot",
                params: { groupId: gid, botId: created.userId },
              })
            : router.back(),
        onError: (e) => fail(e, t("bots.errors.failed")),
      },
    );
  };

  const busy = submit.busy;

  return (
    <Screen testID="screen-add-bot" aboveTabBar={router.inPane}>
      <Header
        onBack={router.onBack}
        title={addingHost ? t("bots.addHostTitle") : t("bots.addTitle")}
        subtitle={addingHost ? bot?.displayName : groupName}
      />
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.xxl }}>
        <Hint>{t("bots.machineNote", { group: groupName })}</Hint>

        <View style={{ gap: space.sm }}>
          <Text style={ty.section}>{addingHost ? t("bots.stepRunHost") : t("bots.stepRun")}</Text>
          <Card style={{ gap: space.md }}>
            <View
              style={{
                backgroundColor: semantic.panel,
                paddingVertical: space.md,
                paddingHorizontal: space.lg,
                borderRadius: r.md,
              }}
            >
              <Text testID="text-bot-pair-command" selectable style={[ty.mono, { color: semantic.text }]}>
                {BOT_PAIR_COMMAND}
              </Text>
            </View>
            <Button
              testID="btn-copy-bot-command"
              surface="raised"
              icon={copyState === "copied" ? <Icon.check size={14} /> : <Icon.copy size={14} />}
              onPress={() => void copy(BOT_PAIR_COMMAND)}
            >
              {copyState === "copied"
                ? t("common:actions.copied")
                : copyState === "failed"
                  ? t("common:actions.copyFailed")
                  : t("common:actions.copy")}
            </Button>
          </Card>
        </View>

        {!addingHost ? (
          <LabeledField label={t("bots.nameLabel")} hint={t("bots.nameHint")}>
            <Field
              testID="input-bot-name"
              accessibilityLabel={t("bots.nameLabel")}
              value={name}
              onChangeText={setName}
              autoCapitalize="words"
              editable={!busy}
              maxLength={64}
            />
          </LabeledField>
        ) : null}

        <LabeledField label={t("bots.codeLabel")} hint={t("bots.codeHint")}>
          <SasCodeField
            testID="input-bot-code"
            accessibilityLabel={t("bots.codeLabel")}
            value={code}
            onChange={(next) => {
              setError(null);
              setCode(next);
            }}
            editable={!busy}
          />
        </LabeledField>

        {error ? <ErrorText testID="text-add-bot-error">{error}</ErrorText> : null}
      </Body>
      <BottomAction>
        <Button
          full
          variant="primary"
          testID="btn-submit-bot"
          disabled={!ready || busy}
          loading={busy}
          onPress={onSubmit}
        >
          {addingHost
            ? busy
              ? t("bots.submittingHost")
              : t("bots.submitHost")
            : busy
              ? t("bots.submittingCreate")
              : t("bots.submitCreate")}
        </Button>
      </BottomAction>
    </Screen>
  );
}

export default observer(AddBot);
