import React from "react";
import { Trans, useTranslation } from "react-i18next";

import { Button } from "../ui/Button";
import { useShortcutLabel } from "../../keyboard";
import type { VoiceInputMode } from "../../types/voice-state";

interface VoiceInputModeSelectProps {
  value: VoiceInputMode;
  onChange: (mode: VoiceInputMode) => void;
}

// Wire values; only labels are translated.
const MODE_OPTIONS: { mode: VoiceInputMode; labelKey: string }[] = [
  { mode: "voice_activity", labelKey: "inputMode.voiceActivity" },
  { mode: "push_to_talk", labelKey: "inputMode.pushToTalk" },
];

/** Input-mode picker (#849). */
export const VoiceInputModeSelect: React.FC<VoiceInputModeSelectProps> = ({
  value,
  onChange,
}) => {
  const { t } = useTranslation("voice");
  // Same resolver as the dispatcher, so a rebound key shows.
  const pttCombo = useShortcutLabel("voice.pushToTalk");

  return (
    <div className="flex flex-col gap-2 max-w-[20rem]" data-testid="voice-input-mode">
      <span className="text-muted">{t("inputMode.heading")}</span>
      <div className="flex gap-2">
        {MODE_OPTIONS.map((opt) => (
          <Button
            key={opt.mode}
            data-testid={`voice-input-mode-${opt.mode}`}
            variant={value === opt.mode ? "primary" : "secondary"}
            size="sm"
            onClick={() => onChange(opt.mode)}
          >
            {t(opt.labelKey)}
          </Button>
        ))}
      </div>
      <span className="text-xs font-mono text-muted">
        {value === "push_to_talk" ? (
          <Trans
            t={t}
            i18nKey="inputMode.pushToTalkHint"
            values={{ combo: pttCombo }}
            components={{ combo: <span className="text-fg" /> }}
          />
        ) : (
          t("inputMode.voiceActivityHint")
        )}
      </span>
    </div>
  );
};
