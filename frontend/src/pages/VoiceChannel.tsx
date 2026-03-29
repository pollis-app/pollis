import React, { useEffect, useState } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { invoke } from "@tauri-apps/api/core";
import { ArrowLeft } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";
import { switchVoiceDevice } from "../hooks/useVoiceChannel";
import { VoiceChannelView } from "../components/Voice/VoiceChannelView";
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

  const isInCall = activeVoiceChannelId === channelId;

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
        <span style={{ flex: 1, color: "var(--c-text)" }}>[v] {channelName}</span>
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
        <div className="flex flex-col gap-1">
          <span style={{ color: "var(--c-text-muted)" }}>
            Noise Gate:{" "}
            <span style={{ color: noiseFloor === 0 ? "var(--c-text-dim)" : "var(--c-text)" }}>
              {noiseFloor === 0 ? "off" : noiseFloor}
            </span>
          </span>
          <input
            type="range"
            min={0}
            max={100}
            step={1}
            value={noiseFloor}
            onChange={(e) => handleNoiseFloor(parseInt(e.target.value, 10))}
            style={{ maxWidth: 320, accentColor: "var(--c-accent)", cursor: "pointer" }}
          />
        </div>
      </div>

      {/* Participant list — only when in a call */}
      {isInCall && <VoiceChannelView />}

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
