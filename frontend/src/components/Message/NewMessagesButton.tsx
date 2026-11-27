import React from "react";
import { useTranslation } from "react-i18next";
import { ArrowDown } from "lucide-react";
import { Button } from "../ui/Button";
import type { Skin } from "../../utils/colorUtils";

interface NewMessagesButtonProps {
  count: number;
  skin: Skin;
  onClick: () => void;
}

/**
 * "N new messages" (#2125): messages that arrived while the reader was scrolled up. Pinned to the
 * bottom of the log's viewport (a zero-height sticky strip, so it never takes room in the log);
 * a click goes to the newest message, and reaching the bottom any other way clears it too.
 */
export const NewMessagesButton: React.FC<NewMessagesButtonProps> = ({ count, skin, onClick }) => {
  const { t } = useTranslation("chat");
  const label = t(skin === "terminal" ? "list.newMessagesTerminal" : "list.newMessages", { count });
  return (
    <div className="pointer-events-none sticky bottom-0 z-10 flex h-0 justify-center">
      <div className="pointer-events-auto -translate-y-full pb-2">
        <div className="motion-popover-up">
          <Button
            size="sm"
            variant="primary"
            activateOn="press"
            data-testid="new-messages-button"
            title={t("list.newMessagesHint")}
            onClick={onClick}
          >
            <ArrowDown size={14} aria-hidden className="size-[0.933rem] shrink-0" />
            {label}
          </Button>
        </div>
      </div>
    </div>
  );
};
