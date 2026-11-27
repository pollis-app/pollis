import React from "react";
import { Trans, useTranslation } from "react-i18next";
import { useParams } from "@tanstack/react-router";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { useKickMember, useGroupMembers } from "../hooks/queries/useGroups";
import { ConfirmPage } from "../components/Layout/ConfirmPage";

export const KickMemberPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const { finish } = useFinishFlow();
  const { groupId, userId } = useParams({ from: "/groups/$groupId/members/$userId/kick" });
  const kickMutation = useKickMember();
  const { data: members = [] } = useGroupMembers(groupId);
  const member = members.find((m) => m.user_id === userId);
  const toMembers = () => finish({ to: "/groups/$groupId/members", params: { groupId } });

  return (
    <ConfirmPage
      title={t("kickMember.pageTitle")}
      testId="kick-member-confirmation"
      question={
        <Trans
          i18nKey="channels:kickMember.confirm"
          values={{ name: member?.username ?? userId }}
          components={{ strong: <strong /> }}
        />
      }
      consequence={t("kickMember.rejoinNotice")}
      error={
        kickMutation.isError
          ? kickMutation.error instanceof Error
            ? kickMutation.error.message
            : t("kickMember.removeFailed")
          : null
      }
      confirmTestId="kick-member-confirm"
      confirmLabel={t("kickMember.confirmButton")}
      pendingLabel={t("kickMember.submitting")}
      isPending={kickMutation.isPending}
      onConfirm={async () => {
        try {
          await kickMutation.mutateAsync({ groupId, userId });
          toMembers();
        } catch {
          // error shown via isError above
        }
      }}
      cancel={{ testId: "kick-member-cancel", onClick: toMembers }}
    />
  );
};
