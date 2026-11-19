import { useTranslation } from "react-i18next";
import { View } from "react-native";
import { Text } from "../Text";
import { Card, SectionTitle } from "../ui";
import { Hint, ErrorText } from "../groups/FormBits";
import { AppNoticeBlock } from "./AppNoticeBlock";
import { AppIdentity } from "./AppIdentity";
import { AppPlainList } from "./AppPlainList";
import { AppPendingForm } from "./AppPendingForm";
import { AppApprovedForm } from "./AppApprovedForm";
import type { AppMarkKind } from "./AppMark";
import { fonts, semantic, type as ty, space } from "../../theme/tokens";
import { useBotApp } from "../../hooks/queries";
import { activeLocale } from "../../i18n";
import { appNotice, type ApprovedAppView } from "../../lib/botApps";

function approvedMarks(a: ApprovedAppView): AppMarkKind[] {
  const marks: AppMarkKind[] = [];
  if (a.development) {
    marks.push("development");
  }
  if (a.cannotSendOutside) {
    marks.push("noEgress");
  }
  if (a.autoUpdate) {
    marks.push("autoUpdate");
  }
  return marks;
}

/**
 * The bot's App on its admin page (#2005), in order: the notice when it is not running, the approved
 * App and what it can do, then either the build waiting for approval (first approval or update) or
 * the controls to narrow the approved App in place.
 */
export function BotAppSection({
  groupId,
  botUserId,
  approverName,
}: {
  groupId: string;
  botUserId: string;
  approverName: (userId: string) => string;
}) {
  const { t } = useTranslation("mobile");
  const { data: view, isLoading, isError } = useBotApp(groupId, botUserId);

  if (isLoading) {
    return <Hint>{t("common:states.loading")}</Hint>;
  }
  if (isError || !view) {
    return <ErrorText testID="text-bot-app-load-failed">{t("bots.app.loadFailed")}</ErrorText>;
  }

  const notice = appNotice(view);
  const approved = view.approved;
  const pending = view.pending;

  return (
    <View testID="section-bot-app" style={{ gap: space.lg }}>
      {notice ? <AppNoticeBlock notice={notice} /> : null}

      {approved ? (
        <>
          <SectionTitle style={{ paddingHorizontal: 0 }}>{t("bots.app.approvedHeading")}</SectionTitle>
          <Card style={{ gap: space.lg }}>
            <AppIdentity
              testID="bot-app-approved"
              manifest={approved.manifest}
              appHash={approved.appHash}
              developerKey={approved.developerKey}
              source={approved.source}
              marks={approvedMarks(approved)}
              approvedLine={t("bots.app.approvedBy", {
                name: approverName(approved.approverUserId),
                date: new Date(approved.issuedAt * 1000).toLocaleDateString(activeLocale()),
                seq: approved.seq,
              })}
            />
            {view.running?.autoUpdated ? (
              <View testID="bot-app-running-auto" style={{ gap: space.xs }}>
                <Text style={[ty.secondary, { color: semantic.text }]}>
                  {t("bots.app.runningAuto", { version: view.running.version })}
                </Text>
                <Text style={ty.section}>{t("bots.app.runningHashLabel")}</Text>
                <Text
                  testID="bot-app-running-hash"
                  selectable
                  style={[ty.secondary, { fontFamily: fonts.mono400, color: semantic.text }]}
                >
                  {view.running.appHash}
                </Text>
              </View>
            ) : null}
          </Card>
          <SectionTitle style={{ paddingHorizontal: 0 }}>{t("bots.app.canDoHeading")}</SectionTitle>
          <Card>
            <AppPlainList testID="list-bot-app-plain" manifest={approved.manifest} grants={approved.grants} />
          </Card>
        </>
      ) : null}

      {pending ? (
        <AppPendingForm
          key={`${pending.appHash}:${pending.manifestHash}:${pending.developerKey ?? ""}:${view.latestSeq}`}
          view={view}
          pending={pending}
        />
      ) : approved ? (
        <AppApprovedForm key={`approved:${approved.seq}`} view={view} approved={approved} />
      ) : (
        <Hint>{t("bots.app.none")}</Hint>
      )}
    </View>
  );
}
