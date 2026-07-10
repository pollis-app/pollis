import React, { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import { invoke } from "../bridge";
import { PageShell } from "../components/Layout/PageShell";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";
import { Button } from "../components/ui/Button";
import {
  preferencesToApmConfig,
  SCREEN_SHARE_FPS_DEFAULT,
  SCREEN_SHARE_FPS_OPTIONS,
  usePreferences,
  type ApmConfig,
  type NoiseSuppressionLevel,
  type PreferencesData,
} from "../hooks/queries/usePreferences";
import { useVoiceTest } from "../hooks/useVoiceTest";
import { voiceSession } from "../voice";
import type { AudioDevice } from "../types";
import { cameraSession, LOCAL_CAMERA_PREVIEW_KEY, friendlyCameraError } from "../camera/cameraSession";
import type { CameraSource } from "../camera/types";
import { RemoteVideoTile } from "../components/Voice/RemoteVideoTile";
import { useMediaPermissions, openPrivacySettings, type PermissionState } from "../hooks/queries/useMediaPermissions";

const VOICE_DEVICES_KEY = "pollis:voice-devices";
const CAMERA_DEVICE_KEY = "pollis:camera-device";

interface DeviceSelectProps {
  label: string;
  // Structural `{ id, name }` so both AudioDevice and CameraSource fit.
  devices: { id: string; name: string }[];
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
      Filters out background hum, fans, and traffic. Higher settings also strip away quieter speech,
      so leave at Moderate unless your room is noisy.
    </span>
  </div>
);

/** fps → one-line use-case hint shown under the framerate selector. */
const SCREEN_SHARE_FPS_HINTS: Record<number, string> = {
  15: "Documents & browsing",
  30: "Standard",
  60: "Motion & gameplay",
};

interface ScreenShareFpsSelectProps {
  value: number;
  onChange: (fps: number) => void;
}

const ScreenShareFpsSelect: React.FC<ScreenShareFpsSelectProps> = ({ value, onChange }) => (
  <div className="flex flex-col gap-2" style={{ maxWidth: 320 }}>
    <span style={{ color: "var(--c-text-muted)" }}>Capture Framerate</span>
    <div className="flex gap-2">
      {SCREEN_SHARE_FPS_OPTIONS.map((fps) => (
        <Button
          key={fps}
          data-testid={`screenshare-fps-${fps}`}
          variant={value === fps ? "primary" : "secondary"}
          size="sm"
          onClick={() => onChange(fps)}
        >
          {fps} fps
        </Button>
      ))}
    </div>
    <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
      {SCREEN_SHARE_FPS_HINTS[value] ?? "Standard"}. Higher is smoother for
      video and gameplay but uses more CPU and bandwidth; drop to 15 fps for
      documents or a constrained machine/network. Takes effect on your next
      screen share.
    </span>
  </div>
);

const PERMISSION_LABEL: Record<PermissionState, string> = {
  granted: "✅ Granted",
  denied: "⛔ Denied",
  notDetermined: "— Not requested",
  perSession: "Managed by the system",
  unsupported: "Unavailable",
};

/** One camera/mic permission row: status + a deep-link to System Settings.
 *  An app can't grant/revoke its own OS grant — only the user can — so the
 *  action is always "take me there", never an in-app toggle (issue #434). The
 *  deep-link only exists where the OS has a per-app privacy model (macOS /
 *  Windows); on Linux (`perSession`) there's nothing to link to. */
const PermissionRow: React.FC<{ label: string; state: PermissionState; onManage: () => void }> = ({
  label,
  state,
  onManage,
}) => {
  const deepLinkable = state === "granted" || state === "denied" || state === "notDetermined";
  return (
    <div className="flex items-center justify-between gap-3" style={{ maxWidth: 320 }}>
      <div className="flex flex-col">
        <span style={{ color: "var(--c-text)" }}>{label}</span>
        <span className="text-xs font-mono" style={{ color: "var(--c-text-muted)" }}>
          {PERMISSION_LABEL[state]}
        </span>
      </div>
      {deepLinkable && (
        <Button variant="secondary" size="sm" onClick={onManage}>
          Manage in System Settings
        </Button>
      )}
    </div>
  );
};

const selectStyle: React.CSSProperties = {
  appearance: "none",
  WebkitAppearance: "none",
  background: "var(--c-surface)",
  color: "var(--c-text)",
  border: "2px solid var(--c-border)",
  padding: "6px 28px 6px 8px",
  fontFamily: "var(--font-mono)",
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
      const ins = devices.filter((d) => d.kind === "input");
      const outs = devices.filter((d) => d.kind === "output");
      setInputs(ins);
      setOutputs(outs);
      // Reset stale prefs: a saved id that's no longer enumerated would make
      // the <select> silently fall back to its first option, so the dropdown
      // shows one device while voice tries to open another. Clear it instead.
      if (selectedInput !== "default" && !ins.some((d) => d.id === selectedInput)) {
        setSelectedInputState("default");
        void voiceSession.setInputDevice("default");
      }
      if (selectedOutput !== "default" && !outs.some((d) => d.id === selectedOutput)) {
        setSelectedOutputState("default");
        void voiceSession.setOutputDevice("default");
      }
    }).catch(() => { });
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const setInput = (id: string) => {
    setSelectedInputState(id);
    void voiceSession.setInputDevice(id);
    if (test.phase !== "idle") {
      test.stopMicTest();
      test.stopPlayback();
    }
  };

  const setOutput = (id: string) => {
    setSelectedOutputState(id);
    void voiceSession.setOutputDevice(id);
    if (test.phase !== "idle") {
      test.stopMicTest();
      test.stopPlayback();
    }
  };

  // ── Camera (issue #434) ───────────────────────────────────────────────────
  // Live self-preview via the preview-only capture path (no call, nothing
  // published). Frames mirror to LOCAL_CAMERA_PREVIEW_KEY; RemoteVideoTile
  // renders + auto-mirrors them. Start on mount / device change, stop on leave.
  const [cameras, setCameras] = useState<CameraSource[]>([]);
  const [selectedCamera, setSelectedCameraState] = useState<string>(() => {
    try { return localStorage.getItem(CAMERA_DEVICE_KEY) || ""; } catch { return ""; }
  });
  const [cameraError, setCameraError] = useState<string | null>(null);

  // OS camera/mic permission status (issue #434) — refetches on window focus, so
  // returning from System Settings reflects the change without a manual refresh.
  const permissions = useMediaPermissions();

  const startCameraPreview = (id: string) => {
    setCameraError(null);
    cameraSession.startPreview(id).catch((e) => {
      setCameraError(friendlyCameraError(String(e)));
    });
  };

  const setCamera = (id: string) => {
    setSelectedCameraState(id);
    try { localStorage.setItem(CAMERA_DEVICE_KEY, id); } catch { /* ignore */ }
    startCameraPreview(id);
  };

  useEffect(() => {
    let cancelled = false;
    cameraSession
      .listDevices()
      .then(({ cameras }) => {
        if (cancelled) { return; }
        setCameras(cameras);
        if (cameras.length === 0) { return; }
        // Preview the saved camera if still present, else the first one.
        const initial = cameras.some((c) => c.id === selectedCamera)
          ? selectedCamera
          : cameras[0].id;
        setSelectedCameraState(initial);
        startCameraPreview(initial);
      })
      .catch((e) => {
        if (!cancelled) { setCameraError(friendlyCameraError(String(e))); }
      });
    return () => {
      cancelled = true;
      void cameraSession.stopPreview();
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  /**
   * Persist a partial preference change and push the resulting APM config to
   * the backend so mid-call changes take effect immediately.
   */
  const savePrefsAndPushApm = (patch: Partial<PreferencesData>) => {
    const next: PreferencesData = { ...preferences.query.data, ...patch };
    preferences.save(next);
    void pushApmConfig(preferencesToApmConfig(next));
  };

  const micBoost = preferences.query.data?.mic_boost_db ?? 0;
  const autoGain = preferences.query.data?.auto_gain_control ?? true;
  const agcTarget = preferences.query.data?.agc_target_dbfs ?? 6;
  const nsLevel: NoiseSuppressionLevel = preferences.query.data?.noise_suppression_level ?? "high";
  const aecEnabled = preferences.query.data?.echo_cancellation ?? true;
  const clickSuppression = preferences.query.data?.click_suppression ?? false;

  const autoJoinVoice = preferences.query.data?.auto_join_voice ?? false;
  const handleAutoJoinVoice = (enabled: boolean) => {
    preferences.save({ ...preferences.query.data, auto_join_voice: enabled });
  };

  const screenShareFps = preferences.query.data?.screen_share_max_fps ?? SCREEN_SHARE_FPS_DEFAULT;
  const handleScreenShareFps = (fps: number) => {
    preferences.save({ ...preferences.query.data, screen_share_max_fps: fps });
  };

  return (
    <PageShell title="Voice & Video" scrollable>
      <div className="flex justify-center px-6 py-8">
      <div className="flex flex-col gap-8 w-full max-w-md">

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

        <section className="flex flex-col gap-4 mb-12" data-testid="voice-camera-section">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
            style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
          >
            Camera
          </h2>
          {cameras.length === 0 ? (
            <span style={{ color: "var(--c-text-muted)" }}>No camera detected.</span>
          ) : (
            <>
              <DeviceSelect
                label="Camera"
                devices={cameras}
                value={selectedCamera}
                onChange={setCamera}
                fallbackLabel="No camera"
              />
              {/* Live self-preview (mirrored). 16:9 letterbox; RemoteVideoTile
                  contains within it and auto-mirrors LOCAL_CAMERA_PREVIEW_KEY. */}
              <div
                data-testid="voice-camera-preview"
                className="flex items-center justify-center overflow-hidden rounded"
                style={{
                  width: "100%",
                  maxWidth: 320,
                  aspectRatio: "16 / 9",
                  background: "#000",
                  border: "1px solid var(--c-border)",
                }}
              >
                {cameraError ? (
                  <span className="px-3 text-center text-sm" style={{ color: "var(--c-text-muted)" }}>
                    {cameraError}
                  </span>
                ) : (
                  <RemoteVideoTile trackKey={LOCAL_CAMERA_PREVIEW_KEY} />
                )}
              </div>
            </>
          )}
        </section>

        {permissions.data && (
          <section className="flex flex-col gap-4 mb-12" data-testid="voice-permissions-section">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
              style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
            >
              Permissions
            </h2>
            <PermissionRow
              label="Camera"
              state={permissions.data.camera}
              onManage={() => { void openPrivacySettings("camera"); }}
            />
            <PermissionRow
              label="Microphone"
              state={permissions.data.microphone}
              onManage={() => { void openPrivacySettings("microphone"); }}
            />
          </section>
        )}

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
              style={{ color: "var(--c-danger)" }}
            >
              {test.error}
            </p>
          )}
        </section>

        <section className="flex flex-col gap-7 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
            style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
          >
            Audio Processing
          </h2>

          <RangeSlider
            label="Microphone Boost"
            value={micBoost}
            onChange={(v) => savePrefsAndPushApm({ mic_boost_db: v })}
            min={0}
            max={20}
            step={1}
            sublabel="Adds extra volume to your mic before anything else processes it. Use this if you're still too quiet even at full system volume."
            description={micBoost === 0 ? "off" : `+${micBoost} dB`}
          />

          <Switch
            label="Auto Volume Leveling"
            checked={autoGain}
            onChange={(enabled) => savePrefsAndPushApm({ auto_gain_control: enabled })}
            description="Keeps your voice at a consistent level — quiet speech is brought up, loud bursts are reined in. Turn off if you'd rather set mic volume yourself."
          />

          <RangeSlider
            label="Auto Volume Target"
            value={agcTarget}
            onChange={(v) => savePrefsAndPushApm({ agc_target_dbfs: v })}
            min={3}
            max={15}
            step={1}
            disabled={!autoGain}
            sublabel="How loud Auto Volume Leveling tries to make you. Lower = louder (3 may clip on a hot mic), higher = quieter. Most people are happy at 6."
            description={`level ${agcTarget}`}
          />

          <NoiseSuppressionSelect
            value={nsLevel}
            onChange={(level) => savePrefsAndPushApm({ noise_suppression_level: level })}
          />

          <Switch
            label="Echo Cancellation"
            checked={aecEnabled}
            onChange={(enabled) => savePrefsAndPushApm({ echo_cancellation: enabled })}
            description="Stops your speaker audio from being picked up by your mic and sent back to others. Leave on unless you're always on headphones."
          />

          <Switch
            label="Click Suppression"
            checked={clickSuppression}
            onChange={(enabled) => savePrefsAndPushApm({ click_suppression: enabled })}
            description="A smarter noise filter that catches keyboard typing and mouse clicks the regular Noise Suppression misses. Uses about 5% of one CPU core. Tip: turn Noise Suppression down to Low or Off when this is on so they don't fight each other."
          />
        </section>

        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest pb-1 border-b"
            style={{ color: "var(--c-text)", borderColor: "var(--c-border)" }}
          >
            Screen Share
          </h2>
          <ScreenShareFpsSelect value={screenShareFps} onChange={handleScreenShareFps} />
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
