import React from "react";
import { Trans, useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { useKickMember, useGroupMembers } from "../hooks/queries/useGroups";
import { Button } from "../components/ui/Button";
import { PageShell } from "../components/Layout/PageShell";

export const KickMemberPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId, userId } = useParams({ from: "/groups/$groupId/members/$userId/kick" });
  const kickMutation = useKickMember();
  const { data: members = [] } = useGroupMembers(groupId);
  const member = members.find((m) => m.user_id === userId);

  return (
    <PageShell title={t("kickMember.pageTitle")}>
      <div className="h-full flex flex-col items-center justify-center gap-4 px-6">
        <p className="text-xs font-mono text-center" style={{ color: "var(--c-text-dim)" }}>
          <Trans
            i18nKey="channels:kickMember.confirm"
            values={{ name: member?.username ?? userId }}
            components={{ strong: <strong /> }}
          />
          <br />
          {t("kickMember.rejoinNotice")}
        </p>
        {kickMutation.isError && (
          <p className="text-xs font-mono" style={{ color: "var(--c-danger)" }}>
            {kickMutation.error instanceof Error
              ? kickMutation.error.message
              : t("kickMember.removeFailed")}
          </p>
        )}
        <div className="flex gap-3">
          <Button
            data-testid="kick-member-confirm"
            variant="danger"
            onClick={async () => {
              try {
                await kickMutation.mutateAsync({ groupId, userId });
                navigate({ to: "/groups/$groupId/members", params: { groupId } });
              } catch {
                // error shown via isError above
              }
            }}
            disabled={kickMutation.isPending}
            isLoading={kickMutation.isPending}
            loadingText={t("kickMember.submitting")}
          >
            {t("kickMember.confirmButton")}
          </Button>
          <Button
            data-testid="kick-member-cancel"
            variant="secondary"
            onClick={() => navigate({ to: "/groups/$groupId/members", params: { groupId } })}
          >
            {t("common:actions.cancel")}
          </Button>
        </div>
      </div>
    </PageShell>
  );
};
