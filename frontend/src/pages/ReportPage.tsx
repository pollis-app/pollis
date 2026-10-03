import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate, useParams, useRouter } from "@tanstack/react-router";
import { observer } from "mobx-react-lite";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { useOtherUserProfile } from "../hooks/queries";
import { useReportUser, type ReportReason } from "../hooks/queries/useBlocks";
import { errorMessage } from "../utils/errorMessage";

const REASONS: ReportReason[] = ["spam", "harassment", "illegal", "other"];

/**
 * Report a user (#1213), from a message's menu or their profile. A page, not a
 * dialog (no modals). Signal-style: Pollis receives the account, the reason
 * and, when reporting a message, the conversation and message ids. Never the
 * message text: it is end-to-end encrypted and stays that way.
 */
export const ReportPage: React.FC = observer(() => {
  const { t } = useTranslation("chat");
  const navigate = useNavigate();
  const router = useRouter();
  const params = useParams({ strict: false }) as {
    userId: string;
    conversationId?: string;
    messageId?: string;
  };
  const { data: profile } = useOtherUserProfile(params.userId);
  const report = useReportUser();
  const [reason, setReason] = useState<ReportReason | null>(null);
  const [done, setDone] = useState<null | { blocked: boolean }>(null);

  const name = profile?.preferred_name || (profile?.username ? `@${profile.username}` : t("report.someone"));
  const aboutMessage = !!params.messageId;

  const goBack = () => {
    if (router.history.length > 1) {
      router.history.back();
    } else {
      navigate({ to: "/" });
    }
  };

  const submit = async (alsoBlock: boolean) => {
    if (!reason) {
      return;
    }
    try {
      await report.mutateAsync({
        reportedId: params.userId,
        reason,
        conversationId: params.conversationId ?? null,
        messageId: params.messageId ?? null,
        alsoBlock,
      });
      setDone({ blocked: alsoBlock });
    } catch {
      // Shown below from report.error.
    }
  };

  return (
    <PageShell title={aboutMessage ? t("report.titleMessage") : t("report.titleUser", { name })} scrollable>
      <div className="flex justify-center px-6 py-12">
        <div className="flex flex-col gap-10 w-full max-w-md" data-testid="report-page">
          {done ? (
            <>
              <div className="flex flex-col gap-3" data-testid="report-done">
                <h1 className="text-xl text-fg">{t("report.doneTitle")}</h1>
                <p className="text-sm text-muted leading-relaxed">
                  {done.blocked ? t("report.doneBlocked", { name }) : t("report.done")}
                </p>
              </div>
              <div>
                <Button data-testid="report-done-button" onClick={goBack}>
                  {t("report.close")}
                </Button>
              </div>
            </>
          ) : (
            <>
              <div className="flex flex-col gap-3">
                <h1 className="text-xl text-fg">{aboutMessage ? t("report.titleMessage") : t("report.titleUser", { name })}</h1>
                <p className="text-sm text-muted leading-relaxed">{t("report.intro")}</p>
              </div>
              <div className="flex flex-col gap-3" role="group" aria-label={t("report.reasonLabel")}>
                <span className="text-xs font-mono uppercase tracking-wide text-muted">{t("report.reasonLabel")}</span>
                <div className="flex flex-col gap-2">
                  {REASONS.map((r) => (
                    <Button
                      key={r}
                      data-testid={`report-reason-${r}`}
                      aria-pressed={reason === r}
                      variant={reason === r ? "primary" : "secondary"}
                      className="w-full justify-start"
                      onClick={() => setReason(r)}
                    >
                      {t(`report.reason.${r}`)}
                    </Button>
                  ))}
                </div>
              </div>
              {report.isError && (
                <p className="text-sm text-danger" data-testid="report-error">
                  {errorMessage(report.error) || t("report.failed")}
                </p>
              )}
              <div className="flex flex-wrap gap-2">
                <Button
                  data-testid="report-submit"
                  disabled={!reason || report.isPending}
                  isLoading={report.isPending && !report.variables?.alsoBlock}
                  onClick={() => submit(false)}
                >
                  {t("report.submit")}
                </Button>
                <Button
                  data-testid="report-and-block"
                  variant="danger"
                  disabled={!reason || report.isPending}
                  isLoading={report.isPending && !!report.variables?.alsoBlock}
                  onClick={() => submit(true)}
                >
                  {t("report.submitAndBlock")}
                </Button>
                <Button variant="ghost" onClick={goBack}>
                  {t("common:actions.cancel")}
                </Button>
              </div>
            </>
          )}
        </div>
      </div>
    </PageShell>
  );
});
