import React, { useState } from "react";
import { useSendGroupInvite } from "../hooks/queries";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";

interface InviteMemberProps {
  groupId: string;
  groupName: string;
}

export const InviteMember: React.FC<InviteMemberProps> = ({ groupId, groupName }) => {
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
        <form
          onSubmit={handleInvite}
          className="w-full max-w-md flex flex-col gap-6"
        >
          <p className="text-xs font-mono" style={{ color: 'var(--c-text-dim)' }}>
            Invite someone to <span style={{ color: 'var(--c-accent)' }}>{groupName}</span>
          </p>

          <div className="flex flex-col gap-3">
            <TextInput
              label="Username or Email"
              value={username}
              onChange={setUsername}
              placeholder="friend@pollis.com"
              disabled={inviteMutation.isPending}
              id="invite-username"
            />

            <Button
              data-testid="send-invite-button"
              type="submit"
              disabled={!username.trim() || inviteMutation.isPending}
              isLoading={inviteMutation.isPending}
              loadingText="Sending…"
            >
              Send Invite
            </Button>
          </div>

          {success && (
            <p data-testid="invite-sent-confirmation" className="text-xs font-mono" style={{ color: 'var(--c-accent-dim)' }}>
              Invite sent.
            </p>
          )}

          {inviteMutation.error && (
            <p data-testid="invite-error" className="text-xs font-mono" style={{ color: '#ff6b6b' }}>
              {inviteMutation.error instanceof Error ? inviteMutation.error.message : "Failed to send invite"}
            </p>
          )}
        </form>
      </div>
    </div>
  );
};
