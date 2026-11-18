import React from "react";
import { Trans, useTranslation } from "react-i18next";
import { useNavigate, useParams } from "@tanstack/react-router";
import { useGroupMembers } from "../hooks/queries/useGroups";
import { useGroupAliases, useResetGroupAlias } from "../hooks/queries/useGroupAliases";
import { aliasName } from "../utils/groupAlias";
import { ConfirmPage } from "../components/Layout/ConfirmPage";

/** An admin resets a member's alias in this group (#2003); they may set it again. */
export const ResetAliasPage: React.FC = () => {
  const { t } = useTranslation("channels");
  const navigate = useNavigate();
  const { groupId, userId } = useParams({ from: "/groups/$groupId/members/$userId/reset-alias" });
  const reset = useResetGroupAlias();
  const { data: members = [] } = useGroupMembers(groupId);
  const aliases = useGroupAliases(groupId);
  const member = members.find((m) => m.user_id === userId);
  const name = aliasName(aliases, userId, member?.username ?? userId);
  const toMembers = () => navigate({ to: "/groups/$groupId/members", params: { groupId } });

  return (
    <ConfirmPage
      title={t("resetAlias.pageTitle")}
      testId="reset-alias-confirmation"
      question={
        <Trans
          i18nKey="channels:resetAlias.confirm"
          values={{ name, handle: member?.username ?? userId }}
          components={{ strong: <strong /> }}
        />
      }
      consequence={t("resetAlias.consequence")}
      error={reset.isError ? t("resetAlias.failed") : null}
      confirmTestId="reset-alias-confirm"
      confirmLabel={t("resetAlias.confirmButton")}
      pendingLabel={t("resetAlias.submitting")}
      isPending={reset.isPending}
      onConfirm={async () => {
        try {
          await reset.mutateAsync({ groupId, userId });
          toMembers();
        } catch {
          // Shown via isError above.
        }
      }}
      cancel={{ testId: "reset-alias-cancel", onClick: toMembers }}
    />
  );
};
