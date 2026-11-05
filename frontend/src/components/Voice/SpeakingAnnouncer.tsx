// Visually hidden `aria-live` region that names who started speaking, throttled so a lively room isn't read out word by word.

import React, { useEffect, useRef, useState } from "react";
import { observer } from "mobx-react-lite";
import { useTranslation } from "react-i18next";

import { appStore } from "../../stores/appStore";
import { userIdFromVoiceIdentity } from "../../voice/identity";

/** Minimum gap between announcements; speakers who start inside it are not announced. */
const THROTTLE_MS = 2000;

export const SpeakingAnnouncer: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  const { voiceActiveSpeakerIds, voiceParticipants } = appStore;
  const [text, setText] = useState("");
  const previous = useRef<Set<string>>(new Set());
  const lastAt = useRef(0);

  const speakingNow = new Set(voiceActiveSpeakerIds);
  const started = voiceActiveSpeakerIds.filter((id) => !previous.current.has(id));
  previous.current = speakingNow;

  // The announcement is derived in render; the state write happens in an effect so render stays pure.
  useEffect(() => {
    if (started.length === 0) {
      return;
    }
    const now = performance.now();
    if (now - lastAt.current < THROTTLE_MS) {
      return;
    }
    const id = started[0];
    const p = voiceParticipants.find((x) => x.identity === id);
    if (!p || p.isLocal) {
      return;
    }
    lastAt.current = now;
    setText(t("stage.startedSpeaking", { name: p.name || userIdFromVoiceIdentity(id) }));
  });

  return (
    <div className="sr-only" aria-live="polite" aria-atomic="true" data-testid="voice-speaking-announcer">
      {text}
    </div>
  );
});
