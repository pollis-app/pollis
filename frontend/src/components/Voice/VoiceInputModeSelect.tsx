import React from "react";

import { Button } from "../ui/Button";
import { useShortcutLabel } from "../../keyboard";
import type { VoiceInputMode } from "../../types/voice-state";

interface VoiceInputModeSelectProps {
  value: VoiceInputMode;
  onChange: (mode: VoiceInputMode) => void;
}

const MODE_OPTIONS: { mode: VoiceInputMode; label: string }[] = [
  { mode: "voice_activity", label: "Voice Activity" },
  { mode: "push_to_talk", label: "Push to Talk" },
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
  // Read through the same resolver the dispatcher uses, so a rebound key
  // shows up here immediately instead of advertising a stale default.
  const pttCombo = useShortcutLabel("voice.pushToTalk");

  return (
    <div className="flex flex-col gap-2 max-w-[20rem]" data-testid="voice-input-mode">
      <span className="text-muted">Input Mode</span>
      <div className="flex gap-2">
        {MODE_OPTIONS.map((opt) => (
          <Button
            key={opt.mode}
            data-testid={`voice-input-mode-${opt.mode}`}
            variant={value === opt.mode ? "primary" : "secondary"}
            size="sm"
            onClick={() => onChange(opt.mode)}
          >
            {opt.label}
          </Button>
        ))}
      </div>
      <span className="text-xs font-mono text-muted">
        {value === "push_to_talk" ? (
          <>
            Your microphone is off until you hold{" "}
            <span className="text-fg">{pttCombo}</span>, and closes again the
            moment you let go — or if the window loses focus. Rebind it in Key
            Bindings.
          </>
        ) : (
          "Your microphone is open whenever you are not muted. Best with headphones and a quiet room."
        )}
      </span>
    </div>
  );
};
