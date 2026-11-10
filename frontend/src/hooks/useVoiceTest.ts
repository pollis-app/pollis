import { errorMessage } from "../utils/errorMessage";
import { useCallback, useEffect, useRef, useState } from "react";
import { Channel, invoke } from "../bridge";

// Mirrors VoiceTestEvent in src-tauri/src/commands/voice_test.rs
type VoiceTestEvent =
  | { type: "frame"; peak: number; rms: number }
  | { type: "recording_started" }
  | { type: "recording_finished" }
  | { type: "playback_started" }
  | { type: "playback_finished" };

type VoiceTestPhase =
  | "idle"
  | "mic_listening"
  | "recording"
  | "playing";

type TonePreset = "sweep" | "chime";

type LevelListener = (peak: number, rms: number) => void;

interface UseVoiceTestResult {
  /** Level frames (~33 Hz) for a ref-driven meter; never React state, so the page doesn't re-render per frame. */
  subscribeLevel: (listener: LevelListener) => () => void;
  monitor: boolean;
  phase: VoiceTestPhase;
  error: string | null;
  startMicTest: (inputDeviceId: string, outputDeviceId: string, monitor: boolean) => Promise<void>;
  stopMicTest: () => Promise<void>;
  setMonitor: (enabled: boolean, outputDeviceId: string) => Promise<void>;
  recordAndPlayBack: (inputDeviceId: string, outputDeviceId: string, durationMs: number) => Promise<void>;
  playTone: (outputDeviceId: string, kind: TonePreset) => Promise<void>;
  stopPlayback: () => Promise<void>;
}

/** Voice Settings pre-flight test: one Channel for level frames and events, plus the voice_test commands. */
export function useVoiceTest(): UseVoiceTestResult {
  const levelListeners = useRef(new Set<LevelListener>());
  const subscribeLevel = useCallback((listener: LevelListener) => {
    levelListeners.current.add(listener);
    return () => {
      levelListeners.current.delete(listener);
    };
  }, []);
  const [phase, setPhase] = useState<VoiceTestPhase>("idle");
  const [monitor, setMonitorState] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const phaseRef = useRef<VoiceTestPhase>("idle");
  const setPhaseBoth = useCallback((next: VoiceTestPhase) => {
    phaseRef.current = next;
    setPhase(next);
  }, []);

  const resetMeter = useCallback(() => {
    for (const listener of levelListeners.current) {
      listener(0, 0);
    }
  }, []);

  useEffect(() => {
    const ch = new Channel<VoiceTestEvent>();
    ch.onmessage = (ev) => {
      switch (ev.type) {
        case "frame":
          for (const listener of levelListeners.current) {
            listener(ev.peak, ev.rms);
          }
          break;
        case "recording_started":
          phaseRef.current = "recording";
          setPhase("recording");
          break;
        case "recording_finished":
          break;
        case "playback_started":
          phaseRef.current = "playing";
          setPhase("playing");
          resetMeter();
          break;
        case "playback_finished":
          phaseRef.current = "idle";
          setPhase("idle");
          resetMeter();
          break;
      }
    };
    invoke("subscribe_voice_test_events", { onEvent: ch }).catch((e) => {
      console.warn("[voice-test] subscribe failed:", e);
    });

    return () => {
      // Leaving the page stops the mic and tones.
      invoke("stop_mic_test").catch((e) => console.warn("stop_mic_test failed", e));
      invoke("stop_test_playback").catch((e) => console.warn("stop_test_playback failed", e));
    };
  }, [resetMeter]);

  const startMicTest = useCallback(
    async (inputDeviceId: string, outputDeviceId: string, mon: boolean) => {
      setError(null);
      resetMeter();
      setMonitorState(mon);
      try {
        await invoke("start_mic_test", {
          inputDeviceId,
          outputDeviceId,
          monitor: mon,
        });
        setPhaseBoth("mic_listening");
      } catch (e) {
        setError(errorMessage(e));
        setPhaseBoth("idle");
      }
    },
    [resetMeter, setPhaseBoth],
  );

  const stopMicTest = useCallback(async () => {
    setError(null);
    try {
      await invoke("stop_mic_test");
    } catch (e) {
      setError(errorMessage(e));
    }
    setPhaseBoth("idle");
    resetMeter();
  }, [resetMeter, setPhaseBoth]);

  const setMonitor = useCallback(
    async (enabled: boolean, outputDeviceId: string) => {
      setMonitorState(enabled);
      try {
        await invoke("set_mic_test_monitor", {
          enabled,
          outputDeviceId,
        });
      } catch (e) {
        setError(errorMessage(e));
      }
    },
    [],
  );

  const recordAndPlayBack = useCallback(
    async (inputDeviceId: string, outputDeviceId: string, durationMs: number) => {
      setError(null);
      resetMeter();
      setPhaseBoth("recording");
      try {
        await invoke("record_and_play_back", {
          inputDeviceId,
          outputDeviceId,
          durationMs,
        });
      } catch (e) {
        setError(errorMessage(e));
        setPhaseBoth("idle");
      }
    },
    [resetMeter, setPhaseBoth],
  );

  const playTone = useCallback(
    async (outputDeviceId: string, kind: TonePreset) => {
      setError(null);
      setPhaseBoth("playing");
      try {
        await invoke("play_test_tone", {
          outputDeviceId,
          kind,
        });
      } catch (e) {
        setError(errorMessage(e));
        setPhaseBoth("idle");
      }
    },
    [setPhaseBoth],
  );

  const stopPlayback = useCallback(async () => {
    setError(null);
    try {
      await invoke("stop_test_playback");
    } catch (e) {
      setError(errorMessage(e));
    }
    setPhaseBoth("idle");
  }, [setPhaseBoth]);

  return {
    subscribeLevel,
    monitor,
    phase,
    error,
    startMicTest,
    stopMicTest,
    setMonitor,
    recordAndPlayBack,
    playTone,
    stopPlayback,
  };
}
