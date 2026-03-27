import React, { useEffect, useState } from "react";
import { useNavigate, useParams } from "@tanstack/react-router";
import { ArrowLeft } from "lucide-react";
import { useAppStore } from "../stores/appStore";
import { useUserGroupsWithChannels } from "../hooks/queries/useGroups";

const VOICE_DEVICES_KEY = "pollis:voice-devices";

interface DeviceSelectProps {
  label: string;
  devices: MediaDeviceInfo[];
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
        background: "var(--c-surface)",
        color: "var(--c-text)",
        border: "2px solid var(--c-border)",
        padding: "6px 8px",
        fontFamily: "inherit",
        fontSize: "inherit",
        outline: "none",
        cursor: "pointer",
        borderRadius: "0.5rem",
        maxWidth: 320,
      }}
      onFocus={(e) => { e.currentTarget.style.borderColor = "var(--c-border-active)"; }}
      onBlur={(e) => { e.currentTarget.style.borderColor = "var(--c-border)"; }}
    >
      {devices.length === 0 ? (
        <option value="default">{fallbackLabel}</option>
      ) : (
        devices.map((d) => (
          <option key={d.deviceId} value={d.deviceId}>
            {d.label || fallbackLabel}
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

  const [inputs, setInputs] = useState<MediaDeviceInfo[]>([]);
  const [outputs, setOutputs] = useState<MediaDeviceInfo[]>([]);
  const [selectedInput, setSelectedInputState] = useState<string>(() => {
    try { return JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || "{}").input || "default"; } catch { return "default"; }
  });
  const [selectedOutput, setSelectedOutputState] = useState<string>(() => {
    try { return JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || "{}").output || "default"; } catch { return "default"; }
  });

  useEffect(() => {
    navigator.mediaDevices.enumerateDevices().then((devices) => {
      setInputs(devices.filter((d) => d.kind === "audioinput"));
      setOutputs(devices.filter((d) => d.kind === "audiooutput"));
    }).catch(() => {});
  }, []);

  const saveDevice = (key: "input" | "output", val: string) => {
    const curr: Record<string, string> = JSON.parse(localStorage.getItem(VOICE_DEVICES_KEY) || "{}");
    localStorage.setItem(VOICE_DEVICES_KEY, JSON.stringify({ ...curr, [key]: val }));
  };

  const setInput = (id: string) => { setSelectedInputState(id); saveDevice("input", id); };
  const setOutput = (id: string) => { setSelectedOutputState(id); saveDevice("output", id); };

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

      {/* Body */}
      <div className="flex flex-col px-4 py-6 gap-5">
        {/* Device selectors */}
        <div className="flex flex-col gap-3">
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
        </div>

        {/* Join / Leave button */}
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
            alignSelf: "flex-start",
            borderRadius: "0.25rem",
          }}
        >
          {isInCall ? "[leave voice]" : "[join voice]"}
        </button>

        {isInCall && (
          <span style={{ color: "var(--c-text-dim)" }}>
            Connected — use the bar below to mute or leave
          </span>
        )}
      </div>
    </div>
  );
};
