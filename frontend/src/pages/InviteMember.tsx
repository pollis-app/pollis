import { errorMessage } from "../utils/errorMessage";
import React, { useState } from "react";
import { Trans, useTranslation } from "react-i18next";
import { useSendGroupInvite } from "../hooks/queries";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { InviteLinkManager } from "../components/Invites/InviteLinkManager";

interface InviteMemberProps {
  groupId: string;
  groupName: string;
}

export const InviteMember: React.FC<InviteMemberProps> = ({ groupId, groupName }) => {
  const { t } = useTranslation("channels");
  const [username, setUsername] = useState("");
  const [success, setSuccess] = useState(false);
  const inviteMutation = useSendGroupInvite();

  const handleInvite = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!username.trim()) {
      return;
    }
    try {
      await inviteMutation.mutateAsync({ groupId, inviteeIdentifier: username.trim() });
      setSuccess(true);
      setUsername("");
      setTimeout(() => setSuccess(false), 4000);
    } catch (err) {
      console.error("Failed to send invite:", err);
    }
  };

  return (
    <div
      data-testid="invite-member-page"
      className="flex-1 flex flex-col overflow-auto"
      style={{ background: 'var(--c-bg)' }}
    >
      <div className="flex-1 flex justify-center overflow-auto px-6 py-8">
        <div className="w-full max-w-md flex flex-col gap-8">
        <form
          onSubmit={handleInvite}
          className="flex flex-col gap-6"
        >
          <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>
            <Trans
              i18nKey="channels:inviteMember.intro"
              values={{ name: groupName }}
              components={{ accent: <span style={{ color: 'var(--c-accent)' }} /> }}
            />
          </p>

          <div className="flex flex-col gap-3">
            <TextInput
              label={t("inviteMember.identifierLabel")}
              value={username}
              onChange={setUsername}
              placeholder={t("inviteMember.identifierPlaceholder")}
              disabled={inviteMutation.isPending}
              id="invite-username"
            />

            <Button
              data-testid="send-invite-button"
              type="submit"
              disabled={!username.trim() || inviteMutation.isPending}
              isLoading={inviteMutation.isPending}
              loadingText={t("inviteMember.submitting")}
            >
              {t("inviteMember.submit")}
            </Button>
          </div>

          {success && (
            <p data-testid="invite-sent-confirmation" className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>
              {t("inviteMember.sent")}
            </p>
          )}

          {inviteMutation.error && (
            <p data-testid="invite-error" className="text-xs font-mono" style={{ color: 'var(--c-danger)' }}>
              {errorMessage(inviteMutation.error, t("inviteMember.sendFailed"))}
            </p>
          )}
        </form>

        <div className="border-t border-line pt-6">
          <InviteLinkManager groupId={groupId} />
        </div>
        </div>
      </div>
    </div>
  );
};
