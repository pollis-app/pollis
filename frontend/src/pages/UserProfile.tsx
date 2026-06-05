import React from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft, MessageCircle, Ban } from "lucide-react";
import { QRCodeSVG } from "qrcode.react";
import { PageShell } from "../components/Layout/PageShell";
import { PresenceAvatar } from "../components/ui/PresenceAvatar";
import { TerminalMenu, type TerminalMenuItem } from "../components/ui/TerminalMenu";
import {
  useOtherUserProfile,
  useSafetyNumber,
  useSetContactVerified,
} from "../hooks/queries/useUserProfile";
import { Button } from "../components/ui/Button";
import { useBlockUser } from "../hooks/queries";
import { useCreateOrGetDMConversation } from "../hooks/queries/useMessages";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";

export const UserProfilePage: React.FC = observer(() => {
  const navigate = useNavigate();
  const { userId } = useParams({ from: "/user/$userId" });
  const currentUser = appStore.currentUser;

  const { data: profile, isLoading } = useOtherUserProfile(userId);
  const { data: safety } = useSafetyNumber(userId);
  const setVerified = useSetContactVerified(userId);
  const blockMutation = useBlockUser();
  const dmMutation = useCreateOrGetDMConversation();

  const isSelf = currentUser?.id === userId;

  const handleBlock = async () => {
    try {
      await blockMutation.mutateAsync(userId);
      navigate({ to: "/dms" });
    } catch (err) {
      console.error("Failed to block user:", err);
    }
  };

  const handleDM = async () => {
    if (!profile?.username) {
      return;
    }
    try {
      const channel = await dmMutation.mutateAsync(profile.username);
      navigate({ to: "/dms/$conversationId", params: { conversationId: channel.id } });
    } catch (err) {
      console.error("Failed to start DM:", err);
    }
  };

  const headlineName =
    profile?.preferred_name || (profile?.username ? `@${profile.username}` : "User");
  const title = profile?.preferred_name || (profile?.username ? `@${profile.username}` : "Profile");

  const items: TerminalMenuItem[] = !profile || isSelf
    ? [
        {
          id: "back",
          label: "Go back",
          icon: <ArrowLeft size={14} />,
          action: () => navigate({ to: "/dms" }),
          type: "system",
          testId: "user-profile-back",
        },
      ]
    : [
        {
          id: "send-message",
          label: "Send Message",
          icon: <MessageCircle size={14} />,
          action: handleDM,
          disabled: dmMutation.isPending,
          testId: "user-profile-dm",
        },
        {
          id: "block",
          label: "Block",
          icon: <Ban size={14} />,
          action: handleBlock,
          disabled: blockMutation.isPending,
          type: "system",
          testId: "user-profile-block",
        },
        { id: "__sep__", label: "", type: "separator" },
        {
          id: "back",
          label: "Go back",
          icon: <ArrowLeft size={14} />,
          action: () => navigate({ to: "/dms" }),
          type: "system",
          testId: "user-profile-back",
        },
      ];

  return (
    <PageShell title={title} scrollable>
      <div data-testid="user-profile-page" className="flex justify-center px-6 py-10">
        <div className="w-full max-w-md flex flex-col gap-6">
          {isLoading ? (
            <span className="text-xs font-mono self-center" style={{ color: "var(--c-text-muted)" }}>
              Loading…
            </span>
          ) : !profile ? (
            <span className="text-xs font-mono self-center" style={{ color: "var(--c-text-muted)" }}>
              User not found
            </span>
          ) : (
            <>
              {/* Header: name on the left, avatar inline on the right.
                  preferred_name takes the headline when set; @username is
                  always shown (as headline if no preferred_name, otherwise
                  as the secondary handle). */}
              <div className="flex items-center justify-between gap-4">
                <div className="flex flex-col min-w-0">
                  <div
                    data-testid="user-profile-headline"
                    className="font-mono text-2xl truncate"
                    style={{ color: "var(--c-accent)" }}
                  >
                    {headlineName}
                  </div>
                  {profile.preferred_name && profile.username && (
                    <div
                      data-testid="user-profile-username"
                      className="font-mono text-xs truncate"
                      style={{ color: "var(--c-text-muted)" }}
                    >
                      @{profile.username}
                    </div>
                  )}
                </div>
                <PresenceAvatar
                  userId={profile.id}
                  avatarKey={profile.avatar_url}
                  size={72}
                  alt={`${headlineName} avatar`}
                  testId="user-profile-avatar"
                  variant="profile"
                />
              </div>

              {!isSelf && safety && (
                <div
                  data-testid="safety-number"
                  className="flex flex-col gap-3 pt-4"
                  style={{ borderTop: "1px solid var(--c-border)" }}
                >
                  <div className="flex items-center justify-between">
                    <span
                      className="font-mono text-xs uppercase tracking-wide"
                      style={{ color: "var(--c-text-muted)" }}
                    >
                      Safety number
                    </span>
                    <span
                      data-testid="safety-status"
                      className="font-mono text-xs"
                      style={{
                        color:
                          safety.status === "verified"
                            ? "var(--c-accent)"
                            : safety.status === "changed"
                              ? "var(--c-danger)"
                              : "var(--c-text-muted)",
                      }}
                    >
                      {safety.status === "verified"
                        ? "Verified"
                        : safety.status === "changed"
                          ? "Changed — re-verify"
                          : "Not verified"}
                    </span>
                  </div>
                  <div className="flex items-start gap-4">
                    <code
                      data-testid="safety-number-digits"
                      className="font-mono text-sm leading-relaxed break-all flex-1"
                      style={{ color: "var(--c-text)" }}
                    >
                      {safety.safety_number}
                    </code>
                    {/* QR rendering disabled — no in-app scanner exists yet.
                        Showing a QR with no way to scan it (no camera capture
                        / decoder) is misleading UX. Re-enable once we have a
                        Scan button + decode flow. */}
                    {/*
                    <div className="flex flex-col items-center gap-1 flex-shrink-0">
                      <div
                        data-testid="safety-number-qr"
                        style={{ background: "var(--c-bg)", padding: 4, borderRadius: 4 }}
                      >
                        <QRCodeSVG
                          value={safety.qr_payload}
                          size={104}
                          bgColor="var(--c-bg)"
                          fgColor="var(--c-accent)"
                          includeMargin={false}
                          marginSize={0}
                        />
                      </div>
                      <span
                        className="font-mono text-2xs"
                        style={{ color: "var(--c-text-muted)" }}
                      >
                        Scan to verify
                      </span>
                    </div>
                    */}
                  </div>
                  {safety.status === "changed" && (
                    <span
                      className="font-mono text-xs"
                      style={{ color: "var(--c-danger)" }}
                    >
                      This contact's identity key changed since you last verified
                      it. Compare the number again out-of-band before trusting it.
                    </span>
                  )}
                  <p
                    className="font-mono text-xs"
                    style={{ color: "var(--c-text-muted)" }}
                  >
                    Compare these digits with {headlineName} over a trusted
                    channel (in person, a call you recognise). If they match,
                    mark this contact verified.
                  </p>
                  <div>
                    <Button
                      variant={safety.status === "verified" ? "secondary" : "primary"}
                      disabled={setVerified.isPending}
                      onClick={() =>
                        setVerified.mutate(safety.status !== "verified")
                      }
                      data-testid="safety-verify-toggle"
                    >
                      {safety.status === "verified"
                        ? "Remove verification"
                        : "Mark verified"}
                    </Button>
                  </div>
                </div>
              )}

              <div style={{ borderTop: "1px solid var(--c-border)" }}>
                <TerminalMenu items={items} onEsc={() => navigate({ to: "/dms" })} />
              </div>
            </>
          )}
        </div>
      </div>
    </PageShell>
  );
});
