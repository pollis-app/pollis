import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { CreatedInviteLinkCard } from "./CreatedInviteLinkCard";
import { InviteLinkRow } from "./InviteLinkRow";
import { errorMessage } from "../../utils/errorMessage";
import {
  useCreateGroupInviteLink,
  useGroupInviteLinks,
  useRevokeGroupInviteLink,
  type CreatedInviteLink,
} from "../../hooks/queries/useGroups";

interface InviteLinkManagerProps {
  groupId: string;
}

/** Catalogue key plus the number it pluralizes on, resolved at render. */
interface PresetOption {
  id: string;
  labelKey: string;
  count: number | null;
}

const EXPIRY_OPTIONS: (PresetOption & { hours: number | null })[] = [
  { id: "24h", labelKey: "inviteLinks.expiryHours", count: 24, hours: 24 },
  { id: "7d", labelKey: "inviteLinks.expiryDays", count: 7, hours: 24 * 7 },
  { id: "30d", labelKey: "inviteLinks.expiryDays", count: 30, hours: 24 * 30 },
  { id: "never", labelKey: "inviteLinks.expiryNever", count: null, hours: null },
];

const USES_OPTIONS: (PresetOption & { uses: number | null })[] = [
  { id: "1", labelKey: "inviteLinks.usesOption", count: 1, uses: 1 },
  { id: "10", labelKey: "inviteLinks.usesOption", count: 10, uses: 10 },
  { id: "unlimited", labelKey: "inviteLinks.usesUnlimited", count: null, uses: null },
];

/**
 * #847 — create, review and revoke a group's invite links. Only expiry, max
 * uses and revoke: each extra knob is another way to misconfigure admission.
 */
export const InviteLinkManager: React.FC<InviteLinkManagerProps> = ({ groupId }) => {
  const { t } = useTranslation("channels");
  const [expiryHours, setExpiryHours] = useState<number | null>(24 * 7);
  const [maxUses, setMaxUses] = useState<number | null>(null);
  const [created, setCreated] = useState<CreatedInviteLink | null>(null);
  const [error, setError] = useState<string | null>(null);

  const { data: links = [] } = useGroupInviteLinks(groupId);
  const createMutation = useCreateGroupInviteLink();
  const revokeMutation = useRevokeGroupInviteLink();

  const handleCreate = async () => {
    setError(null);
    try {
      const link = await createMutation.mutateAsync({
        groupId,
        expiresInHours: expiryHours,
        maxUses,
      });
      setCreated(link);
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const handleRevoke = async (linkId: string) => {
    setError(null);
    try {
      await revokeMutation.mutateAsync({ linkId, groupId });
      // Clear a revoked link from the card so it offers no dead copy button.
      if (created && created.id === linkId) {
        setCreated(null);
      }
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const presetLabel = (opt: PresetOption): string => {
    if (opt.count == null) {
      return t(opt.labelKey);
    }
    return t(opt.labelKey, { count: opt.count });
  };

  return (
    <div data-testid="invite-link-manager" className="flex flex-col gap-4">
      <div>
        <h3 className="text-sm font-semibold text-fg">{t("inviteLinks.heading")}</h3>
        <p className="mt-1 text-xs text-dim">{t("inviteLinks.blurb")}</p>
      </div>

      <div className="flex flex-col gap-3">
        <div>
          <p className="mb-1.5 text-xs text-muted">{t("inviteLinks.expiresAfter")}</p>
          <div
            role="radiogroup"
            aria-label={t("inviteLinks.expiresAfter")}
            className="flex gap-2 flex-wrap"
          >
            {EXPIRY_OPTIONS.map((opt) => {
              const selected = expiryHours === opt.hours;
              const label = presetLabel(opt);
              return (
                <Button
                  key={opt.id}
                  variant={selected ? "primary" : "secondary"}
                  size="sm"
                  aria-label={label}
                  aria-pressed={selected}
                  data-testid={`expiry-option-${opt.hours ?? "never"}`}
                  onClick={() => setExpiryHours(opt.hours)}
                >
                  {label}
                </Button>
              );
            })}
          </div>
        </div>

        <div>
          <p className="mb-1.5 text-xs text-muted">{t("inviteLinks.maximumUses")}</p>
          <div
            role="radiogroup"
            aria-label={t("inviteLinks.maximumUses")}
            className="flex gap-2 flex-wrap"
          >
            {USES_OPTIONS.map((opt) => {
              const selected = maxUses === opt.uses;
              const label = presetLabel(opt);
              return (
                <Button
                  key={opt.id}
                  variant={selected ? "primary" : "secondary"}
                  size="sm"
                  aria-label={label}
                  aria-pressed={selected}
                  data-testid={`uses-option-${opt.uses ?? "unlimited"}`}
                  onClick={() => setMaxUses(opt.uses)}
                >
                  {label}
                </Button>
              );
            })}
          </div>
        </div>
      </div>

      <div>
        <Button
          onClick={handleCreate}
          isLoading={createMutation.isPending}
          loadingText={t("inviteLinks.creating")}
          data-testid="create-invite-link"
        >
          {t("inviteLinks.create")}
        </Button>
      </div>

      {error && (
        <p data-testid="invite-link-error" className="text-xs text-danger">
          {error}
        </p>
      )}

      {created && <CreatedInviteLinkCard link={created} />}

      {links.length > 0 && (
        <div className="flex flex-col gap-2">
          <p className="text-xs text-muted">{t("inviteLinks.existing")}</p>
          {links.map((link) => (
            <InviteLinkRow
              key={link.id}
              link={link}
              onRevoke={handleRevoke}
              isRevoking={revokeMutation.isPending}
            />
          ))}
        </div>
      )}
    </div>
  );
};
