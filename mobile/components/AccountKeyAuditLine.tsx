// The account-key line on a peer's profile (#330, #2114): desktop's
// frontend/src/components/Security/AccountKeyAuditLine.tsx. One line in every state,
// "Checking key…" until the audit lands, so nothing below it moves.

import { View } from "react-native";
import { useTranslation } from "react-i18next";
import { Txt } from "./ui";
import { Icon } from "./icons";
import { semantic, space } from "../theme/tokens";
import {
  ACCOUNT_KEY_LABEL_KEYS,
  type AccountKeyDisplayStatus,
} from "../lib/accountKeyAuditStatus";

// Advisory, never blocking: only `alarm` is warning-toned; getters, so a theme change re-reads them.
function look(status: AccountKeyDisplayStatus): { glyph: typeof Icon.shield; color: string } {
  switch (status) {
    case "checking":
      return { glyph: Icon.shieldEllipsis, color: semantic.muted };
    case "ok":
      return { glyph: Icon.shieldCheck, color: semantic.accent };
    case "pending":
      return { glyph: Icon.clock, color: semantic.muted };
    case "alarm":
      return { glyph: Icon.shieldAlert, color: semantic.warning };
    case "unavailable":
      return { glyph: Icon.shieldQuestion, color: semantic.muted };
  }
}

export function AccountKeyAuditLine({
  status,
  detail,
  testID = "key-audit",
}: {
  status: AccountKeyDisplayStatus;
  /** Shown after the label on `alarm` only, cut to the one line (read in full by screen readers). */
  detail: string;
  testID?: string;
}) {
  const { t } = useTranslation("settings");
  const { glyph: Glyph, color } = look(status);
  const label = t(ACCOUNT_KEY_LABEL_KEYS[status]);
  const text = status === "alarm" && detail ? `${label} · ${detail}` : label;

  return (
    <View
      testID={testID}
      style={{ flexDirection: "row", alignItems: "center", gap: space.xs }}
    >
      <Glyph size={16} color={color} />
      <Txt
        testID={`${testID}-${status}`}
        variant="secondary"
        numberOfLines={1}
        style={{ color, flexShrink: 1 }}
      >
        {text}
      </Txt>
    </View>
  );
}
