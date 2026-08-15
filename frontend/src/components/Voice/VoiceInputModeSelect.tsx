import React from "react";
import { Trans, useTranslation } from "react-i18next";

import { Button } from "../ui/Button";
import { useShortcutLabel } from "../../keyboard";
import type { VoiceInputMode } from "../../types/voice-state";

interface VoiceInputModeSelectProps {
  value: VoiceInputMode;
  onChange: (mode: VoiceInputMode) => void;
}

// The mode values are wire values (persisted in preferences and sent to the
// Rust gate) — only the labels are translated, keyed off the value.
const MODE_OPTIONS: { mode: VoiceInputMode; labelKey: string }[] = [
  { mode: "voice_activity", labelKey: "inputMode.voiceActivity" },
  { mode: "push_to_talk", labelKey: "inputMode.pushToTalk" },
];

/**
 * Input-mode picker for voice settings (#849).
 *
 * Two options, so a button pair rather than a `<select>` — same treatment
 * the capture-framerate setting next door already uses, and it keeps both
 * choices (and which one is active) visible without a click.
 */
export const VoiceInputModeSelect: React.FC<VoiceInputModeSelectProps> = ({
  value,
  onChange,
}) => {
  const { t } = useTranslation("voice");
  // Read through the same resolver the dispatcher uses, so a rebound key
  // shows up here immediately instead of advertising a stale default.
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
