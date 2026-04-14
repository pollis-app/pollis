import { useCallback, useEffect, useRef, useState } from "react";
import { Channel, invoke } from "@tauri-apps/api/core";

// Mirrors VoiceTestEvent in src-tauri/src/commands/voice_test.rs
type VoiceTestEvent =
  | { type: "frame"; peak: number; rms: number; gated: boolean }
  | { type: "recording_started" }
  | { type: "recording_finished" }
  | { type: "playback_started" }
  | { type: "playback_finished" };

export type VoiceTestPhase =
  | "idle"
  | "mic_listening"
  | "recording"
  | "playing";

export type TonePreset = "sweep" | "chime";

interface UseVoiceTestResult {
  peak: number;
  rms: number;
  gated: boolean;
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

/**
 * Drives the Voice Settings pre-flight test harness. Opens a single Tauri
 * Channel to receive level frames and lifecycle events from the Rust side,
 * then exposes a thin wrapper around the voice_test invoke commands.
 *
 * Only one VoiceSettingsPage can mount at a time, so it's fine to own a
 * single channel here rather than a global subscription.
 */
export function useVoiceTest(): UseVoiceTestResult {
  const [peak, setPeak] = useState(0);
  const [rms, setRms] = useState(0);
  const [gated, setGated] = useState(false);
  const [phase, setPhase] = useState<VoiceTestPhase>("idle");
  const [monitor, setMonitorState] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Track the current phase in a ref so event handlers see the latest value
  // without rebinding the channel onmessage closure.
  const phaseRef = useRef<VoiceTestPhase>("idle");
  const setPhaseBoth = useCallback((next: VoiceTestPhase) => {
    phaseRef.current = next;
    setPhase(next);
  }, []);

  const resetMeter = useCallback(() => {
    setPeak(0);
    setRms(0);
    setGated(false);
  }, []);

  useEffect(() => {
    const ch = new Channel<VoiceTestEvent>();
    ch.onmessage = (ev) => {
      switch (ev.type) {
        case "frame":
          setPeak(ev.peak);
          setRms(ev.rms);
          setGated(ev.gated);
          break;
        case "recording_started":
          phaseRef.current = "recording";
          setPhase("recording");
          break;
        case "recording_finished":
          // Transitional — wait for playback_started before flipping phase.
          break;
        case "playback_started":
          phaseRef.current = "playing";
          setPhase("playing");
          break;
        case "playback_finished":
          phaseRef.current = "idle";
          setPhase("idle");
          setPeak(0);
          setRms(0);
          setGated(false);
          break;
      }
    };
    invoke("subscribe_voice_test_events", { onEvent: ch }).catch((e) => {
      console.warn("[voice-test] subscribe failed:", e);
    });

    return () => {
      // Leaving the page: kill anything still running on the Rust side so
      // the mic doesn't stay hot and tones don't keep playing.
      invoke("stop_mic_test").catch(() => {});
      invoke("stop_test_playback").catch(() => {});
    };
  }, []);

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
        setError(e instanceof Error ? e.message : String(e));
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
      setError(e instanceof Error ? e.message : String(e));
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
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [],
  );

  const recordAndPlayBack = useCallback(
    async (inputDeviceId: string, outputDeviceId: string, durationMs: number) => {
      setError(null);
      resetMeter();
      // Optimistic phase — the Rust side will emit RecordingStarted shortly.
      setPhaseBoth("recording");
      try {
        await invoke("record_and_play_back", {
          inputDeviceId,
          outputDeviceId,
          durationMs,
        });
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
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
        setError(e instanceof Error ? e.message : String(e));
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
      setError(e instanceof Error ? e.message : String(e));
    }
    setPhaseBoth("idle");
  }, [setPhaseBoth]);

  return {
    peak,
    rms,
    gated,
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
