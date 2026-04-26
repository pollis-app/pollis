import React, { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import { invoke } from "@tauri-apps/api/core";
import { PageShell } from "../components/Layout/PageShell";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import {
  preferencesToApmConfig,
  usePreferences,
  type ApmConfig,
  type NoiseSuppressionLevel,
  type PreferencesData,
} from "../hooks/queries/usePreferences";
import { switchVoiceDevice } from "../hooks/useVoiceChannel";
import { useVoiceTest } from "../hooks/useVoiceTest";
import type { AudioDevice } from "../types";

const VOICE_DEVICES_KEY = "pollis:voice-devices";

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
        style={selectStyle}
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

interface NoiseSuppressionSelectProps {
  value: NoiseSuppressionLevel;
  onChange: (level: NoiseSuppressionLevel) => void;
}

const NoiseSuppressionSelect: React.FC<NoiseSuppressionSelectProps> = ({ value, onChange }) => (
  <div className="flex flex-col gap-1" style={{ maxWidth: 320 }}>
    <span style={{ color: "var(--c-text-muted)" }}>Noise Suppression</span>
    <div className="relative">
      <select
        value={value}
        onChange={(e) => onChange(e.target.value as NoiseSuppressionLevel)}
        style={selectStyle}
        onFocus={(e) => { e.currentTarget.style.borderColor = "var(--c-border-active)"; }}
        onBlur={(e) => { e.currentTarget.style.borderColor = "var(--c-border)"; }}
      >
        <option value="off">Off</option>
        <option value="low">Low</option>
        <option value="moderate">Moderate</option>
        <option value="high">High</option>
      </select>
      <ChevronDown
        size={14}
        className="absolute right-2 top-1/2 -translate-y-1/2 pointer-events-none"
        style={{ color: "var(--c-text-muted)" }}
      />
    </div>
    <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
      Suppresses ambient hum, fans, keyboard clicks. Higher levels also chew through quieter
      speech, so leave at Moderate unless background noise is bad.
    </span>
  </div>
);

const selectStyle: React.CSSProperties = {
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
};

/**
 * Push the live APM config to the backend if the user is currently in a
 * voice channel. No-op otherwise (the backend command is itself a no-op
 * when no session is active).
 */
async function pushApmConfig(config: ApmConfig): Promise<void> {
  try {
    await invoke("set_voice_audio_processing", { config });
  } catch (e) {
    // Best-effort — the next join_voice_channel will pass the full config
    // anyway, so a transient IPC failure here is harmless.
    console.warn("[VoiceSettings] set_voice_audio_processing failed:", e);
  }
}

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

  useEffect(() => {
    invoke<AudioDevice[]>("list_audio_devices").then((devices) => {
      setInputs(devices.filter((d) => d.kind === "input"));
      setOutputs(devices.filter((d) => d.kind === "output"));
    }).catch(() => { });
  }, []);

  const setInput = (id: string) => {
    setSelectedInputState(id);
    switchVoiceDevice("audioinput", id);
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

  /**
   * Persist a partial preference change and push the resulting APM config to
   * the backend so mid-call changes take effect immediately.
   */
  const savePrefsAndPushApm = (patch: Partial<PreferencesData>) => {
    const next: PreferencesData = { ...preferences.query.data, ...patch };
    preferences.mutation.mutate(next);
    void pushApmConfig(preferencesToApmConfig(next));
  };

  const autoGain = preferences.query.data?.auto_gain_control ?? true;
  const agcTarget = preferences.query.data?.agc_target_dbfs ?? 9;
  const nsLevel: NoiseSuppressionLevel = preferences.query.data?.noise_suppression_level ?? "moderate";
  const aecEnabled = preferences.query.data?.echo_cancellation ?? true;

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
                  background: "var(--c-accent)",
                  transition: "width 60ms linear",
                }}
              />
            </div>

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

          <Switch
            label="Auto Gain Control"
            checked={autoGain}
            onChange={(enabled) => savePrefsAndPushApm({ auto_gain_control: enabled })}
            description="Software AGC raises quiet voice and reins in shouts. Disable if you'd rather control mic level manually at the OS."
          />

          <RangeSlider
            label="AGC Target Loudness"
            value={agcTarget}
            onChange={(v) => savePrefsAndPushApm({ agc_target_dbfs: v })}
            min={6}
            max={15}
            step={1}
            disabled={!autoGain}
            sublabel="Lower = louder. The slider value is dB below full scale; raise it (toward 15) if AGC pumping is audible, lower it (toward 6) if your voice sounds quiet on the receiving end."
            description={`${agcTarget} dBFS`}
          />

          <NoiseSuppressionSelect
            value={nsLevel}
            onChange={(level) => savePrefsAndPushApm({ noise_suppression_level: level })}
          />

          <Switch
            label="Echo Cancellation"
            checked={aecEnabled}
            onChange={(enabled) => savePrefsAndPushApm({ echo_cancellation: enabled })}
            description="Stops the speaker output from being picked up by the mic and bouncing back to the other side. Leave this on unless you're always on headphones."
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
