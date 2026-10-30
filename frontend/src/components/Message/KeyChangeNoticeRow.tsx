import React from "react";
import { useTranslation } from "react-i18next";
import { ShieldAlert } from "lucide-react";
import { formatFullTimestamp, formatTimeOfDay } from "../../utils/format";
import type { Skin } from "../../utils/colorUtils";

interface KeyChangeNoticeRowProps {
  noticeId: number;
  peerUserId: string;
  /** "@name", or undefined while the roster loads. */
  peerLabel?: string;
  at: number;
  skin: Skin;
  onVerify: (peerUserId: string) => void;
}

/** A system line where a peer's safety number changed (#1605), Signal-style; links to the safety number. */
export const KeyChangeNoticeRow: React.FC<KeyChangeNoticeRowProps> = React.memo(({
  noticeId,
  peerUserId,
  peerLabel,
  at,
  skin,
  onVerify,
}) => {
  const { t } = useTranslation("chat");
  const name = peerLabel ?? t("keyChange.defaultName");
  const refined = skin === "refined";
  return (
    <div
      data-testid={`key-change-notice-${noticeId}`}
      data-peer-user-id={peerUserId}
      className={
        refined
          ? "flex items-center justify-center gap-2 px-4 py-2 text-xs text-muted"
          : "flex items-center justify-center gap-2 px-4 py-1 text-xs font-mono text-muted"
      }
    >
      <ShieldAlert size={14} aria-hidden="true" className="flex-shrink-0 text-accent" />
      <span data-testid="key-change-notice-text">
        {t("keyChange.notice", { name })}{" "}
        <button
          type="button"
          data-testid="key-change-notice-verify"
          className="underline bg-transparent text-accent"
          onClick={() => onVerify(peerUserId)}
        >
          {t("keyChange.verify")}
        </button>
      </span>
      <bdi
        dir="ltr"
        title={formatFullTimestamp(at)}
        className={refined ? "font-machine text-2xs tabular-nums" : "tabular-nums"}
      >
        {formatTimeOfDay(at)}
      </bdi>
    </div>
  );
});
KeyChangeNoticeRow.displayName = "KeyChangeNoticeRow";
