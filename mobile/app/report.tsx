import { useState } from "react";
import { Pressable, Text, View } from "react-native";
import { useLocalSearchParams, useRouter } from "expo-router";
import { useTranslation } from "react-i18next";
import { useQuery } from "@tanstack/react-query";
import { Screen, Crumb, Button, BottomAction } from "../components/ui";
import { Heading } from "../components/auth/Heading";
import { semantic, type as ty, r } from "../theme/tokens";
import { upper } from "../i18n";
import { invoke } from "../lib/native";
import { useReportUser, type ReportReason } from "../hooks/queries";

const REASONS: ReportReason[] = ["spam", "harassment", "illegal", "other"];

/**
 * Report a user (#1213), from a message's action sheet or their profile.
 * Signal-style: Pollis receives the account, the reason and, for a message,
 * the conversation and message ids. Never the message text, which stays
 * end-to-end encrypted. One screen: pick a reason, then Report or Report and
 * block; then a short confirmation.
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
      await invoke<{ username?: string; preferred_name?: string } | null>("get_user_profile", { userId }),
    enabled: !!userId,
    staleTime: 1000 * 60,
  });
  const name =
    profile.data?.preferred_name ||
    (profile.data?.username ? `@${profile.data.username}` : t("report.someone"));
  const title = messageId ? t("report.titleMessage") : t("report.titleUser", { name });

  const submit = (alsoBlock: boolean) => {
    if (!reason || !userId) {
      return;
    }
    report.mutate(
      { reportedId: userId, reason, conversationId: conversationId ?? null, messageId: messageId ?? null, alsoBlock },
      { onSuccess: () => setDone({ blocked: alsoBlock }) },
    );
  };

  const crumb = <Crumb segs={[{ label: upper(t("actions.report")), leaf: true }]} />;

  if (done) {
    return (
      <Screen testID="screen-report" centered>
        {crumb}
        <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32 }}>
          <Heading
            testID="report-done"
            title={t("report.doneTitle")}
            subtitle={done.blocked ? t("report.doneBlocked", { name }) : t("report.done")}
          />
        </View>
        <BottomAction>
          <Button testID="btn-report-done" variant="primary" full onPress={() => router.back()}>
            {upper(t("report.close"))}
          </Button>
        </BottomAction>
      </Screen>
    );
  }

  return (
    <Screen testID="screen-report" centered>
      {crumb}
      <View style={{ flex: 1, paddingHorizontal: 24, paddingTop: 32, gap: 28 }}>
        <Heading title={title} subtitle={t("report.intro")} />
        <View style={{ gap: 10 }} accessibilityRole="radiogroup" accessibilityLabel={t("report.reasonLabel")}>
          <Text style={ty.label}>{upper(t("report.reasonLabel"))}</Text>
          {REASONS.map((value) => {
            const selected = reason === value;
            return (
              <Pressable
                key={value}
                testID={`report-reason-${value}`}
                accessibilityRole="radio"
                accessibilityState={{ checked: selected }}
                onPress={() => setReason(value)}
                style={{
                  paddingVertical: 14,
                  paddingHorizontal: 14,
                  borderWidth: 1,
                  borderRadius: r.sm,
                  borderColor: selected ? semantic.accent : semantic.hairStrong,
                  backgroundColor: selected ? semantic.accentSoft : "transparent",
                }}
              >
                <Text style={{ fontFamily: ty.body.fontFamily, fontSize: 14, color: selected ? semantic.accent : semantic.ink }}>
                  {t(`report.reason.${value}`)}
                </Text>
              </Pressable>
            );
          })}
        </View>
        {report.isError ? (
          <Text testID="report-error" style={{ fontFamily: ty.body.fontFamily, fontSize: 13, color: semantic.danger }}>
            {(report.error as Error).message || t("report.failed")}
          </Text>
        ) : null}
      </View>
      <BottomAction>
        <Button
          testID="btn-report-submit"
          variant="primary"
          full
          disabled={!reason || report.isPending}
          onPress={() => submit(false)}
        >
          {upper(t("report.submit"))}
        </Button>
        <Button
          testID="btn-report-and-block"
          variant="danger"
          full
          disabled={!reason || report.isPending}
          onPress={() => submit(true)}
        >
          {upper(t("report.submitAndBlock"))}
        </Button>
      </BottomAction>
    </Screen>
  );
}
