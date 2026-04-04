import React, { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import { useRouter } from "@tanstack/react-router";
import { invoke } from "@tauri-apps/api/core";
import { PageShell } from "../components/Layout/PageShell";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";
import { usePreferences } from "../hooks/queries/usePreferences";
import { switchVoiceDevice } from "../hooks/useVoiceChannel";
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
  const router = useRouter();
  const preferences = usePreferences();

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
  };

  const setOutput = (id: string) => {
    setSelectedOutputState(id);
    switchVoiceDevice("audiooutput", id);
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

  const autoJoinVoice = preferences.query.data?.auto_join_voice ?? true;
  const handleAutoJoinVoice = (enabled: boolean) => {
    preferences.mutation.mutate({ ...preferences.query.data, auto_join_voice: enabled });
  };

  return (
    <PageShell title="Voice Settings" onBack={() => router.history.back()} scrollable>
      <div className="flex flex-col px-6 py-6 gap-6" style={{ maxWidth: 400 }}>

        <section className="flex flex-col gap-4">
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

        <section className="flex flex-col gap-4">
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

        <section className="flex flex-col gap-4">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
            style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
          >
            Behaviour
          </h2>
          <Switch
            label="Auto Join Voice"
            checked={autoJoinVoice}
            onChange={handleAutoJoinVoice}
            description="Automatically join voice when opening a voice channel. Disable to preview who's in the channel before joining."
          />
        </section>

      </div>
    </PageShell>
  );
};
