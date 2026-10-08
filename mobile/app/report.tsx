import { useState } from "react";
import { View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { useQuery } from "@tanstack/react-query";
import {
  Screen,
  Header,
  Body,
  Button,
  BottomAction,
  Group,
  SectionTitle,
  Txt,
} from "../components/ui";
import { ReasonRow } from "../components/direct/ReasonRow";
import { semantic, space } from "../theme/tokens";
import { invoke } from "../lib/native";
import { useReportUser, type ReportReason } from "../hooks/queries";

const REASONS: ReportReason[] = ["spam", "harassment", "illegal", "other"];

/**
 * Report a user (#1213), from a message's action sheet or their profile.
 * Signal-style: Pollis receives the account, the reason and, for a message,
 * the conversation and message ids. Never the message text. One screen:
 * pick a reason, then Report or Report and block; then a short confirmation.
 */
export default function ReportScreen() {
  const { t } = useTranslation("chat");
  const router = useRouter();
  const { userId, conversationId, messageId } = useLocalSearchParams<{
    userId: string;
    conversationId?: string;
    messageId?: string;
  }>();
  const report = useReportUser();
  const [reason, setReason] = useState<ReportReason | null>(null);
  const [done, setDone] = useState<null | { blocked: boolean }>(null);

  const profile = useQuery({
    queryKey: ["user", "profile", userId],
    queryFn: async () =>
      await invoke<{ username?: string; preferred_name?: string } | null>(
        "get_user_profile",
        { userId },
      ),
    enabled: !!userId,
    staleTime: 1000 * 60,
  });
  const name =
    profile.data?.preferred_name ||
    (profile.data?.username
      ? `@${profile.data.username}`
      : t("report.someone"));
  const title = messageId
    ? t("report.titleMessage")
    : t("report.titleUser", { name });

  const submit = (alsoBlock: boolean) => {
    if (!reason || !userId) {
      return;
    }
    report.mutate(
      {
        reportedId: userId,
        reason,
        conversationId: conversationId ?? null,
        messageId: messageId ?? null,
        alsoBlock,
      },
      { onSuccess: () => setDone({ blocked: alsoBlock }) },
    );
  };

  const header = <Header title={t("actions.report")} />;

  if (done) {
    return (
      <Screen testID="screen-report" centered>
        {header}
        <Body contentContainerStyle={{ padding: space.xxl }}>
          <View
            testID="report-done"
            style={{ gap: space.sm, paddingTop: space.xxl }}
          >
            <Txt variant="title" accessibilityRole="header">
              {t("report.doneTitle")}
            </Txt>
            <Txt style={{ color: semantic.dim }}>
              {done.blocked
                ? t("report.doneBlocked", { name })
                : t("report.done")}
            </Txt>
          </View>
        </Body>
        <BottomAction>
          <Button
            testID="btn-report-done"
            variant="primary"
            full
            onPress={() => router.back()}
          >
            {t("report.close")}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

  return (
    <Screen testID="screen-report" centered>
      {header}
      <Body contentContainerStyle={{ padding: space.xxl, gap: space.xxl }}>
        <View style={{ gap: space.sm }}>
          <Txt variant="title" accessibilityRole="header">
            {title}
          </Txt>
          <Txt style={{ color: semantic.dim }}>{t("report.intro")}</Txt>
        </View>
        <View
          style={{ gap: space.sm }}
          accessibilityRole="radiogroup"
          accessibilityLabel={t("report.reasonLabel")}
        >
          <SectionTitle
            style={{ paddingHorizontal: 0, paddingTop: 0, paddingBottom: 0 }}
          >
            {t("report.reasonLabel")}
          </SectionTitle>
          <Group>
            {REASONS.map((value) => (
              <ReasonRow
                key={value}
                testID={`report-reason-${value}`}
                label={t(`report.reason.${value}`)}
                selected={reason === value}
                onPress={() => setReason(value)}
              />
            ))}
          </Group>
        </View>
        {report.isError ? (
          <Txt
            testID="report-error"
            variant="secondary"
            style={{ color: semantic.accent }}
          >
            {(report.error as Error).message || t("report.failed")}
          </Txt>
        ) : null}
      </Body>
      <BottomAction>
        <Button
          testID="btn-report-submit"
          variant="primary"
          full
          disabled={!reason || report.isPending}
          onPress={() => submit(false)}
        >
          {t("report.submit")}
        </Button>
        {/* Report and block: secondary, below — the label says what it does. */}
        <Button
          testID="btn-report-and-block"
          variant="secondary"
          full
          disabled={!reason || report.isPending}
          onPress={() => submit(true)}
        >
          {t("report.submitAndBlock")}
        </Button>
      </BottomAction>
    </Screen>
  );
}
