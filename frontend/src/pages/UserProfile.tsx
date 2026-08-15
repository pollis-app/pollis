import React from "react";
import { useTranslation } from "react-i18next";
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
import { AccountKeyAuditLine } from "../components/Security/AccountKeyAuditLine";
import { useBlockUser, usePeerAuditAccountKey } from "../hooks/queries";
import { useCreateOrGetDMConversation } from "../hooks/queries/useMessages";
import { appStore } from "../stores/appStore";
import { observer } from "mobx-react-lite";

export const UserProfilePage: React.FC = observer(() => {
  const { t } = useTranslation("dms");
  const navigate = useNavigate();
  const { userId } = useParams({ from: "/user/$userId" });
  const currentUser = appStore.currentUser;

  const { data: profile, isLoading } = useOtherUserProfile(userId);
  const { data: safety } = useSafetyNumber(userId);
  const { data: peerAudit } = usePeerAuditAccountKey(userId);
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
    profile?.preferred_name
    || (profile?.username ? `@${profile.username}` : t("profile.fallbackName"));
  const title =
    profile?.preferred_name
    || (profile?.username ? `@${profile.username}` : t("profile.fallbackTitle"));

  const items: TerminalMenuItem[] = !profile || isSelf
    ? [
        {
          id: "back",
          label: t("common:actions.goBack"),
          icon: <ArrowLeft size={14} className="rtl-mirror" />,
          action: () => navigate({ to: "/dms" }),
          type: "system",
          testId: "user-profile-back",
        },
      ]
    : [
        {
          id: "send-message",
          label: t("profile.sendMessage"),
          icon: <MessageCircle size={14} />,
          action: handleDM,
          disabled: dmMutation.isPending,
          testId: "user-profile-dm",
        },
        {
          id: "block",
          label: t("profile.block"),
          icon: <Ban size={14} />,
          action: handleBlock,
          disabled: blockMutation.isPending,
          type: "system",
          testId: "user-profile-block",
        },
        { id: "__sep__", label: "", type: "separator" },
        {
          id: "back",
          label: t("common:actions.goBack"),
          icon: <ArrowLeft size={14} className="rtl-mirror" />,
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
              {t("common:states.loading")}
            </span>
          ) : !profile ? (
            <span className="text-xs font-mono self-center" style={{ color: "var(--c-text-muted)" }}>
              {t("profile.notFound")}
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
                      <bdi>@{profile.username}</bdi>
                    </div>
                  )}
                </div>
                <PresenceAvatar
                  userId={profile.id}
                  avatarKey={profile.avatar_url}
                  size={72}
                  alt={t("profile.avatarAlt", { name: headlineName })}
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
                      {t("profile.safetyNumber")}
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
                        ? t("profile.statusVerified")
                        : safety.status === "changed"
                          ? t("profile.statusChanged")
                          : t("profile.statusUnverified")}
                    </span>
                  </div>
                  <div className="flex items-start gap-4">
                    <code
                      dir="ltr"
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
                      {t("profile.keyChangedWarning")}
                    </span>
                  )}
                  <p
                    className="font-mono text-xs"
                    style={{ color: "var(--c-text-muted)" }}
                  >
                    {t("profile.compareHint", { name: headlineName })}
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
                        ? t("profile.removeVerification")
                        : t("profile.markVerified")}
                    </Button>
                  </div>
                </div>
              )}

              {/* Account-key transparency: advisory audit of this peer's
                  published identity key against the public log (#330). */}
              {!isSelf && peerAudit && (
                <AccountKeyAuditLine
                  status={peerAudit.status}
                  detail={peerAudit.detail}
                  testId="peer-account-key-audit"
                />
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
