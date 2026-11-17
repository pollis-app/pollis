import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { useNavigate } from "@tanstack/react-router";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { useSkin } from "../hooks/queries/usePreferences";
import { errorMessage } from "../utils/errorMessage";
import { useRedeemGroupInviteLink } from "../hooks/queries/useGroups";

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
  const navigate = useNavigate();
  const [token, setToken] = useState(initialToken);
  const [error, setError] = useState<string | null>(null);
  const redeemMutation = useRedeemGroupInviteLink();

  const handleRedeem = React.useCallback(
    async (value: string) => {
      const trimmed = value.trim();
      if (!trimmed) {
        return;
      }
      setError(null);
      try {
        const result = await redeemMutation.mutateAsync({ token: trimmed });
        navigate({ to: "/groups/$groupId", params: { groupId: result.group_id } });
      } catch (err) {
        setError(errorMessage(err));
      }
    },
    [navigate, redeemMutation]
  );

  // #1094: a deep link never redeems on arrival; joining discloses your identity, so it takes a press.
  const isModern = skin === "modern";

  const body = (
    <>
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

      {error && (
        <p data-testid="invite-redeem-error" className="mt-3 text-body text-danger">
          {error}
        </p>
      )}

      <div className="mt-4">
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
      </div>
    </>
  );

  return (
    <div
      data-testid="join-by-invite-page"
      className="flex flex-1 flex-col overflow-auto bg-bg"
    >
      <div className="flex flex-1 justify-center overflow-auto px-6 py-8">
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void handleRedeem(token);
          }}
          className={
            isModern
              ? "w-full max-w-[32rem] rounded-lg border border-line bg-surface p-6"
              : "w-full max-w-[32rem] border border-line bg-surface p-5"
          }
        >
          <h2
            className={
              isModern ? "text-row font-semibold text-fg" : "text-row text-accent"
            }
          >
            {isModern ? t("joinByInvite.heading") : t("joinByInvite.headingTerminal")}
          </h2>
          <p className="mt-1 mb-4 text-secondary text-dim">{t("joinByInvite.blurb")}</p>
          {body}
        </form>
      </div>
    </div>
  );
};
