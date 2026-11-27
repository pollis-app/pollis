import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useFinishFlow } from "../hooks/useFinishFlow";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { PageColumn } from "../components/ui/PageColumn";
import { FormLead } from "../components/ui/FormLead";
import { FormActions } from "../components/ui/FormActions";
import { useSkin } from "../hooks/queries/usePreferences";
import { errorMessage } from "../utils/errorMessage";
import { completeInviteToken } from "../utils/inviteToken";
import { useInviteLinkPreview, useRedeemGroupInviteLink } from "../hooks/queries/useGroups";

interface JoinByInviteProps {
  /** Prefilled when arriving from an /invite/$token deep link. */
  initialToken?: string;
}

/** Redeem an invite link (#847) for `/join` and `/invite/$token`. One message for every failure, as the DS can't distinguish them. */
export const JoinByInvite: React.FC<JoinByInviteProps> = ({
  initialToken = "",
}) => {
  const { t } = useTranslation("channels");
  const skin = useSkin();
  const { finish } = useFinishFlow();
  const [token, setToken] = useState(initialToken);
  const [error, setError] = useState<string | null>(null);
  const redeemMutation = useRedeemGroupInviteLink();
  // #1972: a whole link or code names its group before the press; a partial one is never sent.
  const complete = completeInviteToken(token);
  const preview = useInviteLinkPreview(complete ?? "");
  const groupName = complete ? preview.data?.group_name ?? null : null;
  const showPreviewError = !!complete && preview.isError && !error;

  const handleRedeem = React.useCallback(
    async (value: string) => {
      const trimmed = value.trim();
      if (!trimmed) {
        return;
      }
      setError(null);
      try {
        const result = await redeemMutation.mutateAsync({ token: trimmed });
        finish({ to: "/groups/$groupId", params: { groupId: result.group_id } });
      } catch (err) {
        setError(errorMessage(err));
      }
    },
    [finish, redeemMutation]
  );

  // #1094: a deep link never redeems on arrival; joining discloses your identity, so it takes a press.
  const isModern = skin === "modern";

  const heading = groupName
    ? isModern
      ? t("joinByInvite.headingNamed", { name: groupName })
      : t("joinByInvite.headingNamedTerminal", { name: groupName })
    : isModern
      ? t("joinByInvite.heading")
      : t("joinByInvite.headingTerminal");

  return (
    <PageColumn
      as="form"
      paneTestId="join-by-invite-page"
      onSubmit={(e) => {
        e.preventDefault();
        void handleRedeem(token);
      }}
    >
      <FormLead titleTestId="join-by-invite-heading" title={heading}>
        {t("joinByInvite.blurb")}
      </FormLead>
      <TextInput
        label={
          isModern
            ? t("joinByInvite.tokenLabel")
            : t("joinByInvite.tokenLabelTerminal")
        }
        value={token}
        onChange={setToken}
        placeholder={t("joinByInvite.tokenPlaceholder")}
        description={t("joinByInvite.tokenDescription")}
        disabled={redeemMutation.isPending}
        data-testid="invite-token-input"
        autoFocus
      />

      {(error || showPreviewError) && (
        <p data-testid="invite-redeem-error" className="text-body text-danger">
          {error ?? errorMessage(preview.error)}
        </p>
      )}

      <FormActions>
        <Button
          type="submit"
          disabled={!token.trim()}
          isLoading={redeemMutation.isPending}
          loadingText={
            isModern
              ? t("joinByInvite.submitting")
              : t("joinByInvite.submittingTerminal")
          }
          data-testid="redeem-invite-link"
        >
          {isModern ? t("joinByInvite.submit") : t("joinByInvite.submitTerminal")}
        </Button>
      </FormActions>
    </PageColumn>
  );
};
