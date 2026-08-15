import React, { useState } from "react";
import { useTranslation } from "react-i18next";
import { Button } from "../ui/Button";
import { CreatedInviteLinkCard } from "./CreatedInviteLinkCard";
import { InviteLinkRow } from "./InviteLinkRow";
import { useSkin } from "../../hooks/queries/usePreferences";
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

/**
 * A preset carries the catalogue key plus the number the key pluralizes on, so
 * the labels are resolved by the component rather than snapshotted here at
 * import time.
 */
interface PresetOption {
  id: string;
  labelKey: string;
  count: number | null;
}

/**
 * Expiry presets. A fixed list rather than a date picker, because the point of
 * the feature is that it is not complicated to use — and because every preset
 * here is one a person actually asks for.
 */
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
 * #847 — create, review and revoke a group's shareable invite links.
 *
 * Three controls only: expiry, max uses, revoke. Resisting a fourth is
 * deliberate — each extra knob is another way to misconfigure an admission
 * boundary, and neither Slack nor Discord ships more than this.
 */
export const InviteLinkManager: React.FC<InviteLinkManagerProps> = ({ groupId }) => {
  const { t } = useTranslation("channels");
  const skin = useSkin();
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
      // A revoked link may be the one on screen. Clearing it keeps the card
      // from offering a copy button for a link that no longer works.
      if (created && created.id === linkId) {
        setCreated(null);
      }
    } catch (err) {
      setError(errorMessage(err));
    }
  };

  const isRefined = skin === "refined";
  const optionBase = isRefined
    ? "rounded-md border px-2.5 py-1 text-xs transition-colors"
    : "border px-2.5 py-1 text-xs transition-colors";
  const selectedClass = "border-line-strong bg-surface-high text-accent";
  const unselectedClass = "border-line text-dim hover:bg-hover";

  // A preset with no number ("Never", "Unlimited") has no plural form to pick.
  const presetLabel = (opt: PresetOption): string => {
    if (opt.count == null) {
      return t(opt.labelKey);
    }
    return t(opt.labelKey, { count: opt.count });
  };

  return (
    <div data-testid="invite-link-manager" className="flex flex-col gap-4">
      <div>
        <h3 className={isRefined ? "text-sm font-semibold text-fg" : "text-sm text-accent"}>
          {isRefined ? t("inviteLinks.heading") : t("inviteLinks.headingTerminal")}
        </h3>
        <p className="mt-1 text-xs text-dim">{t("inviteLinks.blurb")}</p>
      </div>

      <div className="flex flex-col gap-3">
        <div>
          <p className="mb-1.5 text-xs text-muted">{t("inviteLinks.expiresAfter")}</p>
          <div className="flex flex-wrap gap-1.5">
            {EXPIRY_OPTIONS.map((opt) => (
              <button
                key={opt.id}
                type="button"
                onClick={() => setExpiryHours(opt.hours)}
                data-testid={`expiry-option-${opt.hours ?? "never"}`}
                className={`${optionBase} ${
                  expiryHours === opt.hours ? selectedClass : unselectedClass
                }`}
              >
                {presetLabel(opt)}
              </button>
            ))}
          </div>
        </div>

        <div>
          <p className="mb-1.5 text-xs text-muted">{t("inviteLinks.maximumUses")}</p>
          <div className="flex flex-wrap gap-1.5">
            {USES_OPTIONS.map((opt) => (
              <button
                key={opt.id}
                type="button"
                onClick={() => setMaxUses(opt.uses)}
                data-testid={`uses-option-${opt.uses ?? "unlimited"}`}
                className={`${optionBase} ${
                  maxUses === opt.uses ? selectedClass : unselectedClass
                }`}
              >
                {presetLabel(opt)}
              </button>
            ))}
          </div>
        </div>
      </div>

      <div>
        <Button
          onClick={handleCreate}
          isLoading={createMutation.isPending}
          loadingText={
            isRefined ? t("inviteLinks.creating") : t("inviteLinks.creatingTerminal")
          }
          data-testid="create-invite-link"
        >
          {isRefined ? t("inviteLinks.create") : t("inviteLinks.createTerminal")}
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
          <p className="text-xs text-muted">
            {isRefined ? t("inviteLinks.existing") : t("inviteLinks.existingTerminal")}
          </p>
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
