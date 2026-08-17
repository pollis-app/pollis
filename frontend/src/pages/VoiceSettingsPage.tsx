import React, { useEffect, useState } from "react";
import { ChevronDown } from "lucide-react";
import { useTranslation } from "react-i18next";
import { invoke } from "../bridge";
import { PageShell } from "../components/Layout/PageShell";
import { RangeSlider } from "../components/ui/RangeSlider";
import { Switch } from "../components/ui/Switch";
import { VoiceInputModeSelect } from "../components/Voice/VoiceInputModeSelect";
import {
  VOICE_INPUT_MODE_DEFAULT,
  type VoiceInputMode,
} from "../types/voice-state";
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
import { voiceSession, readDevicePrefs } from "../voice";
import type { AudioDevice } from "../types";
import { observer } from "mobx-react-lite";
import { cameraSession, LOCAL_CAMERA_PREVIEW_KEY, friendlyCameraError } from "../camera/cameraSession";
import { cameraPreviewStore } from "../camera/cameraPreviewStore";
import { RemoteVideoTile } from "../components/Voice/RemoteVideoTile";
import { useMediaPermissions, openPrivacySettings, type PermissionState } from "../hooks/queries/useMediaPermissions";

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
    <span className="text-muted">{label}</span>
    <div className="relative">
      <select
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className={selectClass}
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
        className="absolute end-2 top-1/2 -translate-y-1/2 pointer-events-none text-muted"
      />
    </div>
  </div>
);

interface NoiseSuppressionSelectProps {
  value: NoiseSuppressionLevel;
  onChange: (level: NoiseSuppressionLevel) => void;
}

// The option values are wire values (persisted in preferences and passed to the
// Rust APM) — only the labels are translated, keyed off the value.
const NOISE_SUPPRESSION_LABEL_KEY: Record<NoiseSuppressionLevel, string> = {
  off: "settings.noiseSuppressionOff",
  low: "settings.noiseSuppressionLow",
  moderate: "settings.noiseSuppressionModerate",
  high: "settings.noiseSuppressionHigh",
};

const NOISE_SUPPRESSION_LEVELS: NoiseSuppressionLevel[] = ["off", "low", "moderate", "high"];

const NoiseSuppressionSelect: React.FC<NoiseSuppressionSelectProps> = ({ value, onChange }) => {
  const { t } = useTranslation("voice");
  return (
    <div className="flex flex-col gap-1" style={{ maxWidth: 320 }}>
      <span className="text-muted">{t("settings.noiseSuppression")}</span>
      <div className="relative">
        <select
          value={value}
          onChange={(e) => onChange(e.target.value as NoiseSuppressionLevel)}
          className={selectClass}
          style={selectStyle}
          onFocus={(e) => { e.currentTarget.style.borderColor = "var(--c-border-active)"; }}
          onBlur={(e) => { e.currentTarget.style.borderColor = "var(--c-border)"; }}
        >
          {NOISE_SUPPRESSION_LEVELS.map((level) => (
            <option key={level} value={level}>
              {t(NOISE_SUPPRESSION_LABEL_KEY[level])}
            </option>
          ))}
        </select>
        <ChevronDown
          size={14}
          className="absolute end-2 top-1/2 -translate-y-1/2 pointer-events-none text-muted"
        />
      </div>
      <span className="text-xs font-mono text-muted">
        {t("settings.noiseSuppressionHint")}
      </span>
    </div>
  );
};

/** fps → the key of the one-line use-case hint shown under the selector. */
const SCREEN_SHARE_FPS_HINT_KEYS: Record<number, string> = {
  15: "settings.fpsHintDocuments",
  30: "settings.fpsHintStandard",
  60: "settings.fpsHintMotion",
};

interface ScreenShareFpsSelectProps {
  value: number;
  onChange: (fps: number) => void;
}

const ScreenShareFpsSelect: React.FC<ScreenShareFpsSelectProps> = ({ value, onChange }) => {
  const { t } = useTranslation("voice");
  return (
    <div className="flex flex-col gap-2" style={{ maxWidth: 320 }}>
      <span className="text-muted">{t("settings.captureFramerate")}</span>
      <div className="flex gap-2">
        {SCREEN_SHARE_FPS_OPTIONS.map((fps) => (
          <Button
            key={fps}
            data-testid={`screenshare-fps-${fps}`}
            variant={value === fps ? "primary" : "secondary"}
            size="sm"
            onClick={() => onChange(fps)}
          >
            {t("settings.fpsOption", { fps })}
          </Button>
        ))}
      </div>
      <span className="text-xs font-mono text-muted">
        {t("settings.fpsHint", {
          hint: t(SCREEN_SHARE_FPS_HINT_KEYS[value] ?? "settings.fpsHintStandard"),
        })}
      </span>
    </div>
  );
};

const PERMISSION_LABEL_KEY: Record<PermissionState, string> = {
  granted: "settings.permissionGranted",
  denied: "settings.permissionDenied",
  notDetermined: "settings.permissionNotRequested",
  perSession: "settings.permissionPerSession",
  unsupported: "settings.permissionUnsupported",
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
  const { t } = useTranslation("voice");
  const deepLinkable = state === "granted" || state === "denied" || state === "notDetermined";
  return (
    <div className="flex items-center justify-between gap-3" style={{ maxWidth: 320 }}>
      <div className="flex flex-col">
        <span className="text-fg">{label}</span>
        <span className="text-xs font-mono text-muted">
          {t(PERMISSION_LABEL_KEY[state])}
        </span>
      </div>
      {deepLinkable && (
        <Button variant="secondary" size="sm" onClick={onManage}>
          {t("settings.manageInSystemSettings")}
        </Button>
      )}
    </div>
  );
};

// The token-backed half of the `<select>` skin — surface fill, foreground text
// and the 2px hairline — as utilities. Paired with `selectStyle` below, which
// keeps only what has no utility equivalent.
const selectClass = "bg-surface text-fg border-2 border-line";

const selectStyle: React.CSSProperties = {
  appearance: "none",
  WebkitAppearance: "none",
  // Logical padding: the trailing 28px reserves room for the caret, which
  // sits on the inline-END edge and therefore swaps sides under `dir=rtl`.
  paddingBlock: "6px",
  paddingInlineStart: "8px",
  paddingInlineEnd: "28px",
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

export const VoiceSettingsPage: React.FC = observer(() => {
  const { t } = useTranslation("voice");
  const preferences = usePreferences();
  const test = useVoiceTest();

  const [inputs, setInputs] = useState<AudioDevice[]>([]);
  const [outputs, setOutputs] = useState<AudioDevice[]>([]);
  const [selectedInput, setSelectedInputState] = useState<string>(
    () => readDevicePrefs().input ?? "default",
  );
  const [selectedOutput, setSelectedOutputState] = useState<string>(
    () => readDevicePrefs().output ?? "default",
  );

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
    }).catch((e) => console.warn("list_audio_devices failed", e));
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

  // ── Camera picker + preview (issue #434) ──────────────────────────────────
  // All state lives in `cameraPreviewStore` — a discriminated union so invalid
  // combos (error beside a live preview, a preview with no device, live while
  // enumerating) are unrepresentable. Preview is OFF by default; the user opts
  // in with the toggle (a settings page must not silently light the camera).
  const cam = cameraPreviewStore.state;

  // OS camera/mic permission status (issue #434) — refetches on window focus, so
  // returning from System Settings reflects the change without a manual refresh.
  const permissions = useMediaPermissions();

  // The invoke resolves once capture has started; drive the union off it — no
  // separate "started" event needed.
  const runPreview = (id: string) => {
    cameraSession
      .startPreview(id)
      .then(() => cameraPreviewStore.wentLive())
      .catch((e) => cameraPreviewStore.failed(friendlyCameraError(String(e))));
  };

  const togglePreview = () => {
    if (cameraPreviewStore.isPreviewing) {
      void cameraSession.stopPreview();
      cameraPreviewStore.stopped();
      return;
    }
    const id = cameraPreviewStore.selectedDeviceId;
    if (!id) { return; }
    cameraPreviewStore.startRequested();
    runPreview(id);
  };

  const setCamera = (id: string) => {
    try { localStorage.setItem(CAMERA_DEVICE_KEY, id); } catch { /* ignore */ }
    const wasPreviewing = cameraPreviewStore.isPreviewing;
    cameraPreviewStore.select(id);
    // `select` kept us in `starting` when previewing — actually restart capture.
    if (wasPreviewing) { runPreview(id); }
  };

  useEffect(() => {
    let cancelled = false;
    const preferred = (() => {
      try { return localStorage.getItem(CAMERA_DEVICE_KEY); } catch { return null; }
    })();
    cameraSession
      .listDevices()
      .then(({ cameras }) => { if (!cancelled) { cameraPreviewStore.enumerated(cameras, preferred); } })
      .catch(() => { if (!cancelled) { cameraPreviewStore.enumerationFailed(); } });
    return () => {
      cancelled = true;
      void cameraSession.stopPreview();
      cameraPreviewStore.reset();
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

  const inputMode: VoiceInputMode =
    preferences.query.data?.voice_input_mode ?? VOICE_INPUT_MODE_DEFAULT;
  // Persist, then push to the Rust gate so a mid-call change takes effect
  // immediately rather than at the next join.
  const handleInputMode = (mode: VoiceInputMode) => {
    preferences.save({ ...preferences.query.data, voice_input_mode: mode });
    void voiceSession.setInputMode(mode);
  };

  const screenShareFps = preferences.query.data?.screen_share_max_fps ?? SCREEN_SHARE_FPS_DEFAULT;
  const handleScreenShareFps = (fps: number) => {
    preferences.save({ ...preferences.query.data, screen_share_max_fps: fps });
  };

  return (
    <PageShell title={t("settings.title")} scrollable>
      <div data-testid="voice-settings-page" className="flex justify-center px-6 py-8">
      <div className="flex flex-col gap-8 w-full max-w-md">

        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("settings.devicesHeading")}
          </h2>
          <DeviceSelect
            label={t("settings.microphone")}
            devices={inputs}
            value={selectedInput}
            onChange={setInput}
            fallbackLabel={t("settings.defaultMicrophone")}
          />
          <DeviceSelect
            label={t("settings.speaker")}
            devices={outputs}
            value={selectedOutput}
            onChange={setOutput}
            fallbackLabel={t("settings.defaultSpeaker")}
          />
        </section>

        <section className="flex flex-col gap-4 mb-12" data-testid="voice-camera-section">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("settings.cameraHeading")}
          </h2>
          {cam.kind === "loading" ? (
            <span className="text-muted">{t("settings.detectingCameras")}</span>
          ) : cam.kind === "empty" ? (
            <span className="text-muted">{t("settings.noCameraDetected")}</span>
          ) : (
            <>
              <DeviceSelect
                label={t("settings.camera")}
                devices={cameraPreviewStore.devices}
                value={cam.deviceId}
                onChange={setCamera}
                fallbackLabel={t("settings.noCameraOption")}
              />
              {/* Self-preview (mirrored). Off by default — the user opts in. 16:9
                  letterbox; RemoteVideoTile contains + auto-mirrors the key. */}
              <div
                data-testid="voice-camera-preview"
                className="flex items-center justify-center overflow-hidden rounded border border-line"
                style={{
                  width: "100%",
                  maxWidth: 320,
                  aspectRatio: "16 / 9",
                  background: "#000",
                }}
              >
                {cam.kind === "live" ? (
                  <RemoteVideoTile trackKey={LOCAL_CAMERA_PREVIEW_KEY} />
                ) : (
                  <span className="px-3 text-center text-sm text-muted">
                    {cam.kind === "failed"
                      ? cam.error
                      : cam.kind === "starting"
                        ? t("settings.starting")
                        : t("settings.previewOff")}
                  </span>
                )}
              </div>
              <div>
                <Button
                  data-testid="voice-camera-preview-toggle"
                  variant={cam.kind === "live" ? "secondary" : "primary"}
                  size="sm"
                  disabled={cam.kind === "starting"}
                  onClick={togglePreview}
                >
                  {cam.kind === "live"
                    ? t("settings.stopCamera")
                    : cam.kind === "starting"
                      ? t("settings.starting")
                      : t("settings.testCamera")}
                </Button>
              </div>
            </>
          )}
        </section>

        {permissions.data && (
          <section className="flex flex-col gap-4 mb-12" data-testid="voice-permissions-section">
            <h2
              className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
            >
              {t("settings.permissionsHeading")}
            </h2>
            <PermissionRow
              label={t("settings.camera")}
              state={permissions.data.camera}
              onManage={() => { void openPrivacySettings("camera"); }}
            />
            <PermissionRow
              label={t("settings.microphone")}
              state={permissions.data.microphone}
              onManage={() => { void openPrivacySettings("microphone"); }}
            />
          </section>
        )}

        <section className="flex flex-col gap-5 mb-12" data-testid="voice-test-section">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("settings.testHeading")}
          </h2>

          {/* ── Microphone test ────────────────────────────────────────── */}
          <div className="flex flex-col gap-2" style={{ maxWidth: 320 }}>
            <span className="text-muted">{t("settings.microphone")}</span>

            {/* Level meter. Reserves its height even when idle so the
                layout doesn't jump on start/stop. */}
            <div
              data-testid="voice-test-meter"
              aria-label={t("settings.micLevelLabel")}
              className="bg-surface border border-line"
              style={{
                height: 12,
                borderRadius: 4,
                overflow: "hidden",
              }}
            >
              <div
                className="bg-accent"
                style={{
                  width: `${Math.max(test.peak, test.rms) * 100}%`,
                  height: "100%",
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
                    ? t("settings.stopMicTest")
                    : t("settings.startMicTest")}
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
                    ? t("settings.recording")
                    : test.phase === "playing"
                      ? t("settings.playing")
                      : t("settings.recordAndPlayBack")}
                </Button>
              </div>
            </div>

            {/* Always rendered so the section height doesn't jump when the
                mic test starts/stops. Disabled unless the mic test is live. */}
            <Switch
              className="mt-4"
              label={t("settings.hearMyself")}
              checked={test.monitor}
              disabled={test.phase !== "mic_listening"}
              onChange={(enabled) => test.setMonitor(enabled, selectedOutput)}
              description={t("settings.hearMyselfDescription")}
            />
          </div>

          {/* ── Speaker test ───────────────────────────────────────────── */}
          <div className="flex flex-col gap-2" style={{ maxWidth: 320 }}>
            <span className="text-muted">{t("settings.speaker")}</span>
            <div className="flex flex-wrap gap-2">
              <Button
                data-testid="voice-test-play-sweep"
                variant="secondary"
                size="sm"
                disabled={test.phase === "playing" || test.phase === "recording"}
                onClick={() => test.playTone(selectedOutput, "sweep")}
              >
                {t("settings.playSweep")}
              </Button>
              <Button
                data-testid="voice-test-play-chime"
                variant="secondary"
                size="sm"
                disabled={test.phase === "playing" || test.phase === "recording"}
                onClick={() => test.playTone(selectedOutput, "chime")}
              >
                {t("settings.playChime")}
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
                {t("settings.stopPlayback")}
              </Button>
            </div>
          </div>

          {test.error && (
            <p
              data-testid="voice-test-error"
              className="text-xs font-mono text-danger"
            >
              {test.error}
            </p>
          )}
        </section>

        <section className="flex flex-col gap-7 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("settings.audioProcessingHeading")}
          </h2>

          <VoiceInputModeSelect value={inputMode} onChange={handleInputMode} />

          <RangeSlider
            label={t("settings.micBoost")}
            value={micBoost}
            onChange={(v) => savePrefsAndPushApm({ mic_boost_db: v })}
            min={0}
            max={20}
            step={1}
            sublabel={t("settings.micBoostSublabel")}
            description={
              micBoost === 0
                ? t("settings.micBoostOff")
                : t("settings.micBoostValue", { db: micBoost })
            }
          />

          <Switch
            label={t("settings.autoVolumeLeveling")}
            checked={autoGain}
            onChange={(enabled) => savePrefsAndPushApm({ auto_gain_control: enabled })}
            description={t("settings.autoVolumeLevelingDescription")}
          />

          <RangeSlider
            label={t("settings.autoVolumeTarget")}
            value={agcTarget}
            onChange={(v) => savePrefsAndPushApm({ agc_target_dbfs: v })}
            min={3}
            max={15}
            step={1}
            disabled={!autoGain}
            sublabel={t("settings.autoVolumeTargetSublabel")}
            description={t("settings.autoVolumeTargetValue", { level: agcTarget })}
          />

          <NoiseSuppressionSelect
            value={nsLevel}
            onChange={(level) => savePrefsAndPushApm({ noise_suppression_level: level })}
          />

          <Switch
            label={t("settings.echoCancellation")}
            checked={aecEnabled}
            onChange={(enabled) => savePrefsAndPushApm({ echo_cancellation: enabled })}
            description={t("settings.echoCancellationDescription")}
          />

          <Switch
            label={t("settings.clickSuppression")}
            checked={clickSuppression}
            onChange={(enabled) => savePrefsAndPushApm({ click_suppression: enabled })}
            description={t("settings.clickSuppressionDescription")}
          />
        </section>

        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("settings.screenShareHeading")}
          </h2>
          <ScreenShareFpsSelect value={screenShareFps} onChange={handleScreenShareFps} />
        </section>

        <section className="flex flex-col gap-4 mb-12">
          <h2
            className="text-xs font-mono font-medium uppercase tracking-widest text-fg pb-1 border-b border-line"
          >
            {t("settings.behaviorHeading")}
          </h2>
          <Switch
            label={t("settings.autoJoinVoice")}
            checked={autoJoinVoice}
            onChange={handleAutoJoinVoice}
            description={t("settings.autoJoinVoiceDescription")}
          />
        </section>

      </div>
      </div>
    </PageShell>
  );
});
