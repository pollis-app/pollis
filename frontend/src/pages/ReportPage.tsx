import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { observer } from "mobx-react-lite";
import { PageShell } from "../components/Layout/PageShell";
import { Button } from "../components/ui/Button";
import { PageColumn } from "../components/ui/PageColumn";
import { FormLead } from "../components/ui/FormLead";
import { FormActions } from "../components/ui/FormActions";
import { useOtherUserProfile } from "../hooks/queries";
import { useReportUser, type ReportReason } from "../hooks/queries/useBlocks";
import { errorMessage } from "../utils/errorMessage";

const REASONS: ReportReason[] = ["spam", "harassment", "illegal", "other"];

/** Report a user (#1213): account, reason and message ids; never the text. */
export const ReportPage: React.FC = observer(() => {
  const { t } = useTranslation("chat");
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

  const { leave: goBack } = useFinishFlow();

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
    }
  };

  return (
    <PageShell title={aboutMessage ? t("report.titleMessage") : t("report.titleUser", { name })} scrollable>
      <PageColumn spacing="sections" data-testid="report-page">
        {done ? (
          <>
            <FormLead testId="report-done" title={t("report.doneTitle")}>
              {done.blocked ? t("report.doneBlocked", { name }) : t("report.done")}
            </FormLead>
            <FormActions>
              <Button data-testid="report-done-button" onClick={goBack}>
                {t("report.close")}
              </Button>
            </FormActions>
          </>
        ) : (
          <>
            <p className="text-body text-muted leading-relaxed">{t("report.intro")}</p>
            <div className="flex flex-col gap-3" role="group" aria-label={t("report.reasonLabel")}>
              <span className="text-secondary font-mono uppercase tracking-wide text-muted">{t("report.reasonLabel")}</span>
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
              <p className="text-body text-danger" data-testid="report-error">
                {errorMessage(report.error) || t("report.failed")}
              </p>
        )}
            <FormActions cancel={{ onClick: goBack }}>
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
            </FormActions>
          </>
        )}
      </PageColumn>
    </PageShell>
  );
});
