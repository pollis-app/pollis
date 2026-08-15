import React, { useState } from "react";
import { useNavigate } from "@tanstack/react-router";
import { TextInput } from "../components/ui/TextInput";
import { Button } from "../components/ui/Button";
import { useSkin } from "../hooks/queries/usePreferences";
import { errorMessage } from "../utils/errorMessage";
import { useRedeemGroupInviteLink } from "../hooks/queries/useGroups";

interface JoinByInviteProps {
  /** Prefilled when arriving from an /invite/$token deep link. */
  initialToken?: string;
  /** Auto-submit on mount — only for the deep-link landing. */
  autoRedeem?: boolean;
}

/**
 * #847 — redeem an invite link.
 *
 * Serves two surfaces with one component: the `/join` page (paste a code) and
 * the `/invite/$token` landing (arrive from a shared link). They differ only in
 * whether the field starts filled, so they share the code rather than drifting.
 *
 * On failure this renders ONE message for every cause. The Delivery Service
 * deliberately cannot tell us whether a token was wrong, expired, revoked or
 * used up — distinguishing them would confirm to an attacker that a token was
 * real but stale. Rendering a friendlier, more specific error here would undo
 * that on the client.
 */
export const JoinByInvite: React.FC<JoinByInviteProps> = ({
  initialToken = "",
  autoRedeem = false,
}) => {
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

  // Deep-link arrival: try immediately so the common case is a single click in
  // the sender's chat app, not a paste. Runs once.
  const autoRan = React.useRef(false);
  React.useEffect(() => {
    if (autoRedeem && initialToken && !autoRan.current) {
      autoRan.current = true;
      void handleRedeem(initialToken);
    }
  }, [autoRedeem, initialToken, handleRedeem]);

  const isRefined = skin === "refined";

  const body = (
    <>
      <TextInput
        label={isRefined ? "Invite link or code" : "INVITE LINK OR CODE"}
        value={token}
        onChange={setToken}
        placeholder="https://pollis.com/invite/…"
        description="Paste the link you were sent, or just the code."
        disabled={redeemMutation.isPending}
        data-testid="invite-token-input"
        autoFocus={!autoRedeem}
      />

      {error && (
        <p data-testid="invite-redeem-error" className="mt-3 text-sm text-danger">
          {error}
        </p>
      )}

      <div className="mt-4">
        <Button
          type="submit"
          disabled={!token.trim()}
          isLoading={redeemMutation.isPending}
          loadingText={isRefined ? "Joining…" : "JOINING…"}
          data-testid="redeem-invite-link"
        >
          {isRefined ? "Join group" : "[JOIN GROUP]"}
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
            isRefined
              ? "w-full max-w-[32rem] rounded-lg border border-line bg-surface p-6"
              : "w-full max-w-[32rem] border border-line bg-surface p-5"
          }
        >
          <h2
            className={
              isRefined ? "text-base font-semibold text-fg" : "text-base text-accent"
            }
          >
            {isRefined ? "Join a group" : "> JOIN A GROUP"}
          </h2>
          <p className="mt-1 mb-4 text-xs text-dim">
            Invite links let you join without an admin looking you up first.
          </p>
          {body}
        </form>
      </div>
    </div>
  );
};
