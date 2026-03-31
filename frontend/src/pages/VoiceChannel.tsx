import React, { useEffect, useState } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { invoke } from "@tauri-apps/api/core";
import { ArrowLeft, Circle, Volume2 } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { switchVoiceDevice } from "../hooks/useVoiceChannel";
import { VoiceChannelView } from "../components/Voice/VoiceChannelView";
import { RangeSlider } from "../components/ui/RangeSlider";
import { useVoiceParticipants } from "../hooks/queries/useVoiceParticipants";
import { usePreferences } from "../hooks/queries/usePreferences";
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
  <div className="flex flex-col gap-1">
    <span style={{ color: "var(--c-text-muted)" }}>{label}</span>
    <select
      value={value}
      onChange={(e) => onChange(e.target.value)}
      style={{
        background: "var(--c-bg)",
        color: "var(--c-text)",
        border: "2px solid var(--c-border)",
        padding: "6px 8px",
        fontFamily: "inherit",
        fontSize: "inherit",
        outline: "none",
        cursor: "pointer",
        borderRadius: "0.5rem",
        maxWidth: 320,
        colorScheme: "dark",
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
  </div>
);

export const VoiceChannelPage: React.FC = () => {
  const navigate = useNavigate();
  const { groupId, channelId } = useParams({ from: "/groups/$groupId/voice/$channelId" });
  const { activeVoiceChannelId, setActiveVoiceChannelId } = useAppStore();

  const { data: groupsWithChannels } = useUserGroupsWithChannels();
  const group = groupsWithChannels?.find((g) => g.id === groupId);
  const channel = group?.channels.find((c) => c.id === channelId);
  const channelName = channel?.name ?? "general";

  const preferences = usePreferences();

  const [inputs, setInputs] = useState<AudioDevice[]>([]);
  const [outputs, setOutputs] = useState<AudioDevice[]>([]);
  const [selectedInput, setSelectedInputState] = useState<string>(() => {
    try { return JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || "{}").input || "default"; } catch { return "default"; }
  });
  const [selectedOutput, setSelectedOutputState] = useState<string>(() => {
    try { return JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || "{}").output || "default"; } catch { return "default"; }
  });

  // Noise gate: 0–100 UI value maps to 0.0–0.10 f32 threshold in Rust.
  const [noiseFloor, setNoiseFloorState] = useState<number>(() => {
    const saved = localStorage.getItem(NOISE_FLOOR_KEY);
    return saved ? parseInt(saved, 10) : 0;
  });

  const autoGain = preferences.query.data?.auto_gain_control ?? true;

  useEffect(() => {
    invoke<AudioDevice[]>('list_audio_devices').then((devices) => {
      setInputs(devices.filter((d) => d.kind === "input"));
      setOutputs(devices.filter((d) => d.kind === "output"));
    }).catch(() => { });
  }, []);

  // Sync saved noise floor to Rust on mount so it's applied from the start.
  useEffect(() => {
    invoke('set_noise_floor', { threshold: noiseFloor / 1000 }).catch(() => { });
  }, []);

  const setInput = (id: string) => {
    setSelectedInputState(id);
    switchVoiceDevice('audioinput', id);
  };

  const setOutput = (id: string) => {
    setSelectedOutputState(id);
    switchVoiceDevice('audiooutput', id);
  };

  const handleNoiseFloor = (val: number) => {
    setNoiseFloorState(val);
    localStorage.setItem(NOISE_FLOOR_KEY, val.toString());
    invoke('set_noise_floor', { threshold: val / 1000 }).catch(() => { });
  };

  const handleAutoGain = (enabled: boolean) => {
    preferences.mutation.mutate({
      ...preferences.query.data,
      auto_gain_control: enabled,
    });
  };

  const isInCall = activeVoiceChannelId === channelId;
  const { data: observerParticipants = [] } = useVoiceParticipants(isInCall ? null : channelId);

  return (
    <div className="flex flex-col h-full font-mono text-xs">
      {/* Header */}
      <div
        className="flex items-center px-4 py-2 flex-shrink-0"
        style={{ borderBottom: "1px solid var(--c-border)", color: "var(--c-text-muted)" }}
      >
        <button
          onClick={() => navigate({ to: "/groups/$groupId", params: { groupId } })}
          className="mr-3 inline-flex items-center gap-1 leading-none transition-colors"
          style={{ color: "var(--c-text-muted)" }}
          onMouseEnter={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-accent)"; }}
          onMouseLeave={(e) => { (e.currentTarget as HTMLElement).style.color = "var(--c-text-muted)"; }}
        >
          <ArrowLeft size={12} />
        </button>
        <span style={{ flex: 1, color: "var(--c-text)" }} className="flex items-center gap-1.5">
          <Volume2 size={12} />
          {channelName}
        </span>
      </div>

      {/* Device selectors and noise gate — always visible */}
      <div className="flex flex-col px-4 py-4 gap-3 flex-shrink-0">
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

        {/* Noise gate slider */}
        <div style={{ maxWidth: 320 }}>
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
        </div>

        {/* Auto gain control toggle */}
        <div style={{ maxWidth: 320 }} className="flex flex-col gap-1">
          <label className="flex items-center gap-2 cursor-pointer">
            <input
              type="checkbox"
              checked={autoGain}
              onChange={(e) => handleAutoGain(e.target.checked)}
              style={{
                width: 14,
                height: 14,
                cursor: "pointer",
                accentColor: "var(--c-accent)",
              }}
            />
            <span style={{ color: "var(--c-text)" }}>Auto Gain Control</span>
          </label>
          <span style={{ color: "var(--c-text-muted)", fontSize: "0.85em" }}>
            Automatically adjusts microphone volume for consistent output levels. Disable if you experience "pumping" effects or prefer manual gain control.
          </span>
        </div>
      </div>

      {/* Participant list */}
      {isInCall ? (
        <VoiceChannelView />
      ) : (
        <div className="flex-1 overflow-auto px-4 py-2 flex flex-col gap-1 font-mono text-xs" style={{ borderTop: "1px solid var(--c-border)", borderBottom: "1px solid var(--c-border)" }}>
          {observerParticipants.length === 0 ? (
            <span style={{ color: "var(--c-text-dim)" }}>No one in this channel</span>
          ) : (
            observerParticipants.map((p) => (
              <div
                key={p.identity}
                className="flex items-center gap-2"
                style={{
                  color: "var(--c-text)",
                  borderLeft: "2px solid transparent",
                  paddingLeft: "6px",
                }}
              >
                <span
                  className="text-lg"
                  style={{ color: "var(--c-border)", lineHeight: 1.25, flexShrink: 0, display: "flex", alignItems: "center" }}
                >
                  <Circle size={12} fill="var(--c-border)" />
                </span>
                <span className="flex-1 truncate">{p.name}</span>
              </div>
            ))
          )}
        </div>
      )}

      {/* Join / Leave button — always visible */}
      <div className="px-4 pb-6 flex-shrink-0">
        <button
          data-testid="voice-join-leave-button"
          onClick={() => isInCall ? setActiveVoiceChannelId(null) : setActiveVoiceChannelId(channelId)}
          style={{
            background: isInCall ? "transparent" : "var(--c-accent)",
            color: isInCall ? "#ff6b6b" : "black",
            border: isInCall ? "2px solid #ff6b6b" : "2px solid transparent",
            padding: "8px 20px",
            fontFamily: "inherit",
            fontSize: "inherit",
            fontWeight: "bold",
            cursor: "pointer",
            letterSpacing: "0.05em",
            borderRadius: "0.25rem",
          }}
        >
          {isInCall ? "Leave" : "Join"}
        </button>
      </div>
    </div>
  );
};
