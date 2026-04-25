import React, { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { PageShell } from "../components/Layout/PageShell";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import { usePreferences } from "../hooks/queries/usePreferences";
import { switchVoiceDevice } from "../hooks/useVoiceChannel";
import { useVoiceTest } from "../hooks/useVoiceTest";
import type { AudioDevice } from "../types";

const VOICE_DEVICES_KEY = "pollis:voice-devices";
const NOISE_FLOOR_KEY = "pollis:noise-floor";

interface DeviceSelectProps {
  label: string;
  devices: AudioDevice[];
  value: string;
  onChange: (id: string) => void;
  fallbackLabel: string;
}

const DeviceSelect: React.FC<DeviceSelectProps> = ({ label, devices, value, onChange, fallbackLabel }) => (
  <div className="flex flex-col gap-1" style={{ maxWidth: 320 }}>
    <span style={{ color: "var(--c-text-muted)" }}>{label}</span>
    <div className="relative">
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        style={{
          appearance: "none",
          WebkitAppearance: "none",
          background: "var(--c-surface)",
          color: "var(--c-text)",
          border: "2px solid var(--c-border)",
          padding: "6px 28px 6px 8px",
          fontFamily: "inherit",
          fontSize: "inherit",
          outline: "none",
          cursor: "pointer",
          borderRadius: "0.5rem",
          width: "100%",
        }}
        onFocus={(e) => { e.currentTarget.style.borderColor = "var(--c-border-active)"; }}
        onBlur={(e) => { e.currentTarget.style.borderColor = "var(--c-border)"; }}
      >
        {devices.length === 0 ? (
          <option value="default">{fallbackLabel}</option>
        ) : (
          devices.map((d) => (
            <option key={d.id} value={d.id}>
              {d.name}
            </option>
          ))
        )}
      </select>
      <ChevronDown
        size={14}
        className="absolute right-2 top-1/2 -translate-y-1/2 pointer-events-none"
        style={{ color: "var(--c-text-muted)" }}
      />
    </div>
  </div>
);

export const VoiceSettingsPage: React.FC = () => {
  const preferences = usePreferences();
  const test = useVoiceTest();

  const [inputs, setInputs] = useState<AudioDevice[]>([]);
  const [outputs, setOutputs] = useState<AudioDevice[]>([]);
  const [selectedInput, setSelectedInputState] = useState<string>(() => {
    try { return JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || "{}").input || "default"; } catch { return "default"; }
  });
  const [selectedOutput, setSelectedOutputState] = useState<string>(() => {
    try { return JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || "{}").output || "default"; } catch { return "default"; }
  });

  const [noiseFloor, setNoiseFloorState] = useState<number>(() => {
    const saved = localStorage.getItem(NOISE_FLOOR_KEY);
    return saved ? parseInt(saved, 10) : 0;
  });

  useEffect(() => {
    invoke<AudioDevice[]>("list_audio_devices").then((devices) => {
      setInputs(devices.filter((d) => d.kind === "input"));
      setOutputs(devices.filter((d) => d.kind === "output"));
    }).catch(() => { });
  }, []);

  // Sync saved noise floor to Rust on mount so it's applied from the start.
  useEffect(() => {
    invoke("set_noise_floor", { threshold: noiseFloor / 1000 }).catch(() => { });
  }, []);

  const setInput = (id: string) => {
    setSelectedInputState(id);
    switchVoiceDevice("audioinput", id);
    // Stop any running test so it doesn't keep hitting the stale device.
    if (test.phase !== "idle") {
      test.stopMicTest();
      test.stopPlayback();
    }
  };

  const setOutput = (id: string) => {
    setSelectedOutputState(id);
    switchVoiceDevice("audiooutput", id);
    if (test.phase !== "idle") {
      test.stopMicTest();
      test.stopPlayback();
    }
  };

  const handleNoiseFloor = (val: number) => {
    setNoiseFloorState(val);
    localStorage.setItem(NOISE_FLOOR_KEY, val.toString());
    invoke("set_noise_floor", { threshold: val / 1000 }).catch(() => { });
  };

  const autoGain = preferences.query.data?.auto_gain_control ?? true;
  const handleAutoGain = (enabled: boolean) => {
    preferences.mutation.mutate({ ...preferences.query.data, auto_gain_control: enabled });
  };

  const autoJoinVoice = preferences.query.data?.auto_join_voice ?? false;
  const handleAutoJoinVoice = (enabled: boolean) => {
    preferences.mutation.mutate({ ...preferences.query.data, auto_join_voice: enabled });
  };

  return (
    <PageShell title="Voice Settings" scrollable>
      <div className="flex justify-center px-6 py-8">
      <div className="flex flex-col gap-8 w-full" style={{ maxWidth: 400 }}>

        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
            style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
          >
            Devices
          </h2>
          <DeviceSelect
            label="Microphone"
            devices={inputs}
            value={selectedInput}
            onChange={setInput}
            fallbackLabel="Default microphone"
          />
          <DeviceSelect
            label="Speaker"
            devices={outputs}
            value={selectedOutput}
            onChange={setOutput}
            fallbackLabel="Default speaker"
          />
        </section>

        <section className="flex flex-col gap-5 mb-12" data-testid="voice-test-section">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
            style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
          >
            Test
          </h2>

          {/* ── Microphone test ────────────────────────────────────────── */}
          <div className="flex flex-col gap-2" style={{ maxWidth: 320 }}>
            <span style={{ color: "var(--c-text-muted)" }}>Microphone</span>

            {/* Level meter. Reserves its height even when idle so the
                layout doesn't jump on start/stop. */}
            <div
              data-testid="voice-test-meter"
              aria-label="Microphone level"
              style={{
                height: 12,
                background: "var(--c-surface)",
                borderRadius: 4,
                overflow: "hidden",
                border: "1px solid var(--c-border)",
              }}
            >
              <div
                style={{
                  width: `${Math.max(test.peak, test.rms) * 100}%`,
                  height: "100%",
                  background: test.gated ? "var(--c-text-muted)" : "var(--c-accent)",
                  transition: "width 60ms linear",
                }}
              />
            </div>
            {/* Always rendered so toggling visibility doesn't shift the rest of
                the section as gating ticks on/off mid-speech. */}
            <span
              className="text-xs font-mono"
              aria-hidden={!(test.phase === "mic_listening" && test.gated)}
              style={{
                color: "var(--c-text-muted)",
                opacity: test.phase === "mic_listening" && test.gated ? 1 : 0,
              }}
            >
              below noise gate — raise your voice or lower the gate
            </span>

            <div className="flex flex-col gap-2 mt-4">
              <div className="flex">
                <Button
                  data-testid={
                    test.phase === "mic_listening"
                      ? "voice-test-stop-mic"
                      : "voice-test-start-mic"
                  }
                  variant="secondary"
                  size="sm"
                  disabled={
                    test.phase === "recording" || test.phase === "playing"
                  }
                  onClick={() =>
                    test.phase === "mic_listening"
                      ? test.stopMicTest()
                      : test.startMicTest(selectedInput, selectedOutput, false)
                  }
                >
                  {test.phase === "mic_listening"
                    ? "Stop mic test"
                    : "Start mic test"}
                </Button>
              </div>
              <div className="flex">
                <Button
                  data-testid="voice-test-record-playback"
                  variant="secondary"
                  size="sm"
                  disabled={test.phase === "recording" || test.phase === "playing" || test.phase === "mic_listening"}
                  onClick={() =>
                    test.recordAndPlayBack(selectedInput, selectedOutput, 3000)
                  }
                >
                  {test.phase === "recording"
                    ? "Recording…"
                    : test.phase === "playing"
                      ? "Playing…"
                      : "Record 3s & play back"}
                </Button>
              </div>
            </div>

            {/* Always rendered so the section height doesn't jump when the
                mic test starts/stops. Disabled unless the mic test is live. */}
            <Switch
              className="mt-4"
              label="Hear myself (may echo)"
              checked={test.monitor}
              disabled={test.phase !== "mic_listening"}
              onChange={(enabled) => test.setMonitor(enabled, selectedOutput)}
              description="Loops the mic back through the selected output. Use headphones to avoid feedback."
            />
          </div>

          {/* ── Speaker test ───────────────────────────────────────────── */}
          <div className="flex flex-col gap-2" style={{ maxWidth: 320 }}>
            <span style={{ color: "var(--c-text-muted)" }}>Speaker</span>
            <div className="flex flex-wrap gap-2">
              <Button
                data-testid="voice-test-play-sweep"
                variant="secondary"
                size="sm"
                disabled={test.phase === "playing" || test.phase === "recording"}
                onClick={() => test.playTone(selectedOutput, "sweep")}
              >
                Play sweep
              </Button>
              <Button
                data-testid="voice-test-play-chime"
                variant="secondary"
                size="sm"
                disabled={test.phase === "playing" || test.phase === "recording"}
                onClick={() => test.playTone(selectedOutput, "chime")}
              >
                Play chime
              </Button>
              {/* Always rendered so the row doesn't grow/shrink when a tone
                  starts/stops. Disabled when there's nothing to stop. */}
              <Button
                data-testid="voice-test-stop-playback"
                variant="secondary"
                size="sm"
                disabled={test.phase !== "playing"}
                onClick={() => test.stopPlayback()}
              >
                Stop
              </Button>
            </div>
          </div>

          {test.error && (
            <p
              data-testid="voice-test-error"
              className="text-xs font-mono"
              style={{ color: "#ff6b6b" }}
            >
              {test.error}
            </p>
          )}
        </section>

        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
            style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
          >
            Audio Processing
          </h2>
          <RangeSlider
            label="Noise Gate (level)"
            value={noiseFloor}
            onChange={handleNoiseFloor}
            min={0}
            max={100}
            step={1}
            sublabel="Filters ambient noise before it's transmitted. Raise if background sounds are triggering your speaking indicator."
            description="0 = off"
          />
          <Switch
            label="Auto Gain Control"
            checked={autoGain}
            onChange={handleAutoGain}
            description="Automatically adjusts microphone volume for consistent output levels. Disable if you experience &quot;pumping&quot; effects or prefer manual gain control."
          />
        </section>

        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
            style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
          >
            Behavior
          </h2>
          <Switch
            label="Auto Join Voice"
            checked={autoJoinVoice}
            onChange={handleAutoJoinVoice}
            description="Automatically join voice when opening a voice channel. Disable to preview who's in the channel before joining."
          />
        </section>

      </div>
      </div>
    </PageShell>
  );
};
