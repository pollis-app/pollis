# Audio Processing

The voice channel mic-side pipeline is owned by us end-to-end via the
[`webrtc-audio-processing`](https://crates.io/crates/webrtc-audio-processing)
crate (PulseAudio's repackaged WebRTC AudioProcessing module). Replaces
libwebrtc's internal APM, which we cannot tune from Rust.

## What APM does

One `Processor` instance per voice session handles:

- **HPF** — high-pass filter, removes mains hum and AC noise.
- **NS** — noise suppression, configurable Off / Low / Moderate / High (default High).
- **AGC2 AdaptiveDigital** — modern WebRTC AGC: speech-presence-gated, up to +30 dB boost for quiet talkers, with a built-in noise-floor limiter (`max_output_noise_level_dbfs` = −50). Configurable headroom (default 6 dB; lower = louder).
- **AEC3** — full echo canceller with internal delay estimation.

libwebrtc's `AudioSourceOptions { echo_cancellation, noise_suppression, auto_gain_control }` are all set to `false` at the LiveKit `NativeAudioSource` so the signal is touched exactly once.

## Pipeline

```text
Capture (mic):
  cpal i16 mono callback (10ms chunks, 48 kHz preferred)
    └─→ frame_task: rebuffer to exact APM frame size (rate / 100 samples)
        └─→ apm.process_capture_frame()         // AGC + NS + HPF + AEC capture side
            └─→ NativeAudioSource.capture_frame  // → LiveKit publish

Render (mixed playback, what's about to hit the speaker):
  per remote track: NativeAudioStream
    └─→ drain_task: i16 → f32, push to track_buffers[track_key]
        ├─→ speaking detection (peak-based)
        └─→ (no other consumers)

  mixer_task (10ms tick):
    └─→ drain ≤480 f32 samples from each track_buffers entry
        └─→ sum, soft-clip to [-1, 1]
            ├─→ apm.analyze_render_frame()          // AEC reference
            └─→ output_ring (interleaved across cpal output channels)
                └─→ cpal output stream
```

Single shared cpal output stream + single mixer is a deliberate change from
the previous per-track-stream model. AEC needs *one* point at which the
about-to-play signal is observed; otherwise the render reference is out of
sync with what actually comes out of the speaker.

## Source files

- `src-tauri/src/commands/voice_apm.rs` — `ApmStage`, `ApmConfig`, helpers (`run_capture`, `analyze_render`).
- `src-tauri/src/commands/voice.rs` — pipeline wiring: `start_mic_stream`, `start_speaker_stream`, `run_drain_task`, `run_mixer_task`, `ensure_playback`, `register_remote_track`. Tauri commands `join_voice_channel` / `set_voice_audio_processing` / `set_voice_input_device` / `set_voice_output_device`.
- `frontend/src/hooks/queries/usePreferences.ts` — `ApmConfig`, `preferencesToApmConfig`, `APM_DEFAULTS`.
- `frontend/src/pages/VoiceSettingsPage.tsx` — UI surface (AGC switch + target slider, NS dropdown, AEC switch). Mid-call changes push via `set_voice_audio_processing`.

## Sample-rate model

APM is locked to the cpal mic input rate at construction. WebRTC supports 8 / 16 / 32 / 48 kHz. The pipeline:

- prefers 48 kHz everywhere.
- will use 16 / 24 kHz if the mic only advertises that (Bluetooth SCO on macOS / Linux).
- disables APM for the session if the mic returns anything else (very rare; e.g. legacy 44.1 kHz USB cards). The session falls back to libwebrtc's internal APM in that case — the configurable submodules just stop applying.

Speaker is asked for the same rate as the mic; `start_speaker_stream` falls back to the device's native rate if it can't honour the request. When speaker rate ≠ APM rate the AEC render reference would be aliased, so the mixer skips `analyze_render_frame` for that session and AEC runs without a reference (capture-side adaptation only).

## Frame sizes

| Rate    | Frame samples (10 ms) |
|---------|-----------------------|
| 48 kHz  | 480                   |
| 32 kHz  | 320                   |
| 16 kHz  | 160                   |
| 8 kHz   | 80                    |

`voice_apm::frame_samples(rate)` computes this. APM panics on size mismatches, so both capture and render paths must hit the exact size.

## Stream delay / AEC

We run AEC3 in `EchoCanceller::Full { stream_delay_ms: None }` — APM3's internal delay estimator is good enough on typical desktop sound stacks (30 – 80 ms mic→speaker round-trip on Linux, 20 – 40 ms on macOS, varies on Windows). Setting a manual stream delay is only worth it once we have calibrated numbers per host; the current default lets the estimator run.

`Processor::set_output_will_be_muted` is called from `toggle_voice_mute` so AGC / AEC don't adapt to silence frames during mute windows.

## Configuration surface

`ApmConfig` (Rust ↔ wire JSON, no rename):

| Field             | Type          | Default  | Notes |
|-------------------|---------------|----------|-------|
| `agc_enabled`     | bool          | `true`   | mirrors the existing `auto_gain_control` preference |
| `agc_target_dbfs` | u8            | `6`      | AGC2 `headroom_db`; UI exposes 3..=15 (lower = louder) |
| `ns_level`        | enum (string) | `"high"` | `"off" \| "low" \| "moderate" \| "high"` |
| `aec_enabled`     | bool          | `true`   | |

Per-user persistence is via `usePreferences` keys `auto_gain_control` / `agc_target_dbfs` / `noise_suppression_level` / `echo_cancellation`. `preferencesToApmConfig(prefs)` projects them into the wire shape.

Changing any of these mid-call invokes `set_voice_audio_processing`, which calls `ApmStage::set_config`. Internal echo / noise / AGC state is preserved across config changes — only the changed submodule re-initialises.

## Build dependencies

`webrtc-audio-processing` is included with the `bundled` feature. The system pkg-config branch wants `webrtc-audio-processing-2 ≥ 2.1` which most distros don't ship yet, and we'd rather every platform build from the same source.

The vendored build runs meson + ninja on the C++ source. CI installs:

- **Linux** (`apt`): `cmake clang meson ninja-build` (in addition to existing GTK/audio deps).
- **macOS** (`brew`): `meson ninja` (clang/cmake come with Xcode CLT).
- **Windows** (`choco`): `meson ninja` (cmake / MSVC come with VS2022 on the runner).

For local Linux dev: `pacman -S meson ninja clang cmake` (Arch) or the equivalent for your distro.

## Where APM does *not* run

- **Voice Settings test harness** (`commands/voice_test.rs`). The mic test, record/playback, and tone playback all bypass APM intentionally: users want to verify the device is delivering audio at all, not what it sounds like after processing. The level meter shows the raw pre-APM signal.
- **Speaker monitor loopback** (the "Hear myself" toggle). Same reason — diagnostic only.

## Known gaps

- No resampler in the AEC reference path. Speaker-rate ≠ APM-rate sessions run without a render reference (logged at session start). Adding `rubato` for these edge cases is on the radar but unnecessary for the 99% case.
- No per-host calibration of `stream_delay_ms`; we let APM3 estimate. Worth profiling per-platform before tuning.
- Transient (keyboard/click) suppression is not available in this APM version — the crate hardcodes `transient_suppression.enabled = false` because upstream WebRTC deprecated it. For click suppression we'd need an ML denoiser (RNNoise, NSNet, etc.) layered upstream, which the issue scoped out.

---
_Back to [index.md](./index.md)_
