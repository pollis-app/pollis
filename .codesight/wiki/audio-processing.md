# Audio Processing

The voice channel audio pipeline is owned by us end-to-end, on both sides of
LiveKit. It runs at a **fixed 48 kHz** internally (`voice::resampler::MIX_RATE_HZ`)
whatever the devices do; the only places the device rate exists are the two cpal
callbacks and the resamplers right behind them.

Mic side, two processing stages, both pre-LiveKit:

- **RNNoise** (via [`nnnoiseless`](https://crates.io/crates/nnnoiseless)) — optional ML denoiser, off by default. Handles transients APM misses.
- **APM** (via [`webrtc-audio-processing`](https://crates.io/crates/webrtc-audio-processing)) — the PulseAudio repackage of WebRTC's AudioProcessing module. AGC2 + spectral NS + HPF + AEC. Replaces libwebrtc's internal APM, which we cannot tune from Rust.

> **Windows: APM does not run.** `webrtc-audio-processing-sys` has no MSVC support
> upstream, so on Windows `ApmStage` compiles against a **no-op `Processor`**
> (`#[cfg(target_os = "windows")]` in `voice_apm.rs`). Construction still succeeds and
> the requested config is still stored and reported — so settings round-trip and the UI
> behaves — but **no frame is processed**: no AGC, no NS, no HPF, no AEC. Windows users
> get raw mic audio into LiveKit. This is a real platform gap, not a detail; everything
> below describes the Linux/macOS path unless it says otherwise.

## What APM does

One `Processor` instance per voice session handles:

- **HPF** — high-pass filter, removes mains hum and AC noise.
- **NS** — noise suppression, configurable Off / Low / Moderate / High (default High).
- **AGC2 AdaptiveDigital** — modern WebRTC AGC: speech-presence-gated, up to +30 dB boost for quiet talkers, with a built-in noise-floor limiter (`max_output_noise_level_dbfs` = −50). Configurable headroom (default 6 dB; lower = louder).
- **AEC3** — full echo canceller with internal delay estimation.

libwebrtc's `AudioSourceOptions { echo_cancellation, noise_suppression, auto_gain_control }` are all set to `false` at the LiveKit `NativeAudioSource` so the signal is touched exactly once.

## Pipeline

```text
Capture (mic) — `voice/streams.rs` + `voice/capture.rs`:
  cpal input callback (device rate, any format/channels; 10 ms period requested)
    └─→ downmix to mono i16 into a reused scratch, push into an rtrb SPSC ring
        (never gated here; a full ring drops the newest samples and counts them)
        └─→ "pollis-mic-dsp" thread, unparked per period:
            ├─→ backlog > 100 ms? discard down to 20 ms (counted)
            ├─→ pop one 10 ms frame at the device rate
            ├─→ FrameResampler → 480 samples @ 48 kHz   (passthrough when the mic is 48 kHz)
            ├─→ DenoiserStage::process                  // RNNoise (when click_suppression on)
            ├─→ apm.process_capture_frame()             // AGC2 + NS + HPF + AEC capture side
            ├─→ local meter → LevelsHub, speaking detection (only while transmitting)
            ├─→ gate: muted / PTT-idle / deafened ⇒ the frame is ZEROED, not dropped
            └─→ NativeAudioSource.capture_frame          // queue_size_ms = 0: direct, 10 ms frames

Render (remote playback) — `voice/playback.rs` + `voice/mixer.rs`:
  per remote track: NativeAudioStream @ 48 kHz mono (libwebrtc resamples)
    └─→ drain task: meter → LevelsHub, speaking detection, i16 → f32,
        push into the track's own rtrb SPSC ring (300 ms capacity, drops counted)

  cpal output callback (device rate, 10 ms period requested) — `OutputMixer::fill`:
    for each 10 ms mix frame it needs:
      ├─→ for every TrackSlot: pre-roll / whole-frame pull / concealment / drift control
      │   (× per-user gain, an atomic on the slot)
      ├─→ sum → SoftLimiter (soft knee at −1 dBFS; bit-exact unity below it)
      ├─→ deafen ⇒ zeros
      ├─→ apm.analyze_render_frame()                     // mic AEC reference
      ├─→ screenshare::self_echo::analyze_render()       // shared-audio AEC
      └─→ FrameResampler 48 kHz → device rate, interleave to the device channels
```

There is no intermediate clock on the render side: the sound card's callback is
the only consumer, pulling whole 10 ms frames from each track's ring. That is
what makes the AEC reference exact (it is literally what is about to hit the
speaker) and removes the phase drift between a timer and the card that used to
produce periodic clicks.

The single tap serves a second consumer. A Linux screen share capturing system
audio picks up this same mix — the call itself — off the sink monitor, so it
subtracts it using this mix as the reference (`VoiceState::shared_audio_render`;
see `capture-split.md` → "Shared audio"). The slot is `None` whenever no such
share is running, so the cost is one uncontended lock per frame.

### Per-track playback state (`mixer::TrackSlot`)

- **Pre-roll**: a track contributes nothing until 20 ms (2 frames) is queued, then
  plays with a 1 ms fade-in.
- **Underrun**: if a primed track has less than a whole frame, the previous frame is
  replayed once with a linear fade to silence, and the track goes back to waiting
  for a pre-roll. Never a partial frame padded with zeros.
- **Drift**: libwebrtc's playout clock and the card's clock differ by ppm. Above 60 ms
  queued a *quiet* frame (peak < −40 dBFS) is dropped; above 120 ms any frame is.
  Counted in `TrackStats`, logged at unsubscribe.
- **Output switch**: `set_voice_output_device` only replaces the cpal stream.
  `MixerShared` (rings, slots, gains, the APM tap) outlives every stream, so every
  participant keeps playing; the new stream's first callback trims each ring back to
  the pre-roll so nothing stale is played.
- **Volume**: `set_remote_user_volume` stores the gain on `PlaybackState::user_volumes`
  (so later tracks of that user inherit it) and writes the atomic on every live slot.
  No map is cloned per tick.

### Device buffers

`streams::buffer_candidates` asks cpal for a 10 ms period — Linux
`BufferSize::Fixed(rate/25)` (ALSA: 40 ms buffer, 10 ms period; PipeWire's ALSA
plugin accepts it, measured 10.7 ms callbacks versus 42.7 ms with `Default`),
macOS `Fixed(rate/100)`, Windows `Fixed(rate/50)` — and falls back to `Default`
if the device refuses. The chosen configuration is logged at open. No
environment variables are set.

### Gating and mute

`TransmitGate` (`voice/gate.rs`) is the state machine; `lifecycle::publish_gate`
mirrors it onto atomics. The DSP thread reads `transmit_muted` per frame and sends
**silent** frames while it is set. Frames must keep flowing: libwebrtc's encoder is
driven by captured frames (no silence padder with a zero-length source queue), and
zeros are what let Opus DTX collapse the stream to one packet every 400 ms. Because
every stage still runs, AEC3 keeps its delay estimate and AGC its gain across
mute/unmute, and the local meter keeps moving so the UI can say "you're muted".

Only the explicit mute (which deafen implies) is signalled to LiveKit as a
publication mute (`apply_gate_to_room`). PTT release just goes silent — no
signalling round trip per key press, no `muted`/`unmuted` flicker for remote
peers.

### Level meters

Each source (local mic, every remote track) feeds `levels::LevelsHub`, which
coalesces the latest bands of everyone into **one** `VoiceEvent::AudioBands { levels }`
per 50 ms tick, rounded to 2 dp. A tick in which everyone is silent is sent once
(so meters fall) and then suppressed until something moves. The emission is
driven by whichever source crosses the tick boundary — no timer.

## Listen-only join (no mic)

The mic is **best-effort**. If `open_mic` fails at join — no capture device, ALSA
`snd_pcm_hw_params` "No such file or directory", a busy device —
`join_voice_channel` does **not** abort. It joins *listen-only*: connected to
the room, receiving remote audio/video, just not publishing a mic track. On
that path the whole capture side is skipped (no `NativeAudioSource`, no
`publish_track`, no DSP thread, no APM, no denoiser); playback is unaffected.
`voice.local_track` / `audio_source` / `input_stream` / `frame_task` are all
`None`.

The backend emits `VoiceEvent::MicAvailability { available: false }` once at
join. The renderer mirrors it onto `voiceState.joined.micAvailable` and swaps
the mute toggle for a non-interactive "listening only" indicator
(`voice-bar-listen-only` / `voice-tray-listen-only`).

Test seam: `POLLIS_DISABLE_MIC=1` forces the mic open to fail so the
listen-only path can be exercised without unplugging a device. Covered by
`e2e/voice-channel-no-mic.js` (S-NOMIC). Upgrading a listen-only session to
speaking (plug a mic in mid-call) currently requires a rejoin —
`set_voice_input_device` only re-opens a stream when `audio_source` is already
`Some`.

## Publish options

`lifecycle::mic_publish_options`: `TrackSource::Microphone`, `AudioEncoding { max_bitrate: 64_000 }`,
DTX on. RED is requested by default but LiveKit forces it off under E2EE (the
frame cryptor and redundancy don't compose), so loss recovery is Opus in-band
FEC plus DTX silence.

## Source files

- `pollis-core/src/commands/voice_apm.rs` — `ApmStage`, `ApmConfig`, helpers (`run_capture_into`, `analyze_render`).
- `pollis-core/src/commands/voice_denoiser.rs` — `DenoiserStage` wrapping `nnnoiseless::DenoiseState`. 48 kHz, which the pipeline always is.
- `pollis-core/src/commands/voice/streams.rs` — cpal openers (`build_mic_stream`, `build_output_stream`, the buffer-size candidates) plus the ring-fed wrappers the Voice Settings test harness uses.
- `pollis-core/src/commands/voice/capture.rs` — the mic ring + DSP thread (`open_mic`, `spawn_capture_task`, `CaptureTask`), shared by join and `set_voice_input_device`.
- `pollis-core/src/commands/voice/resampler.rs` — `FrameResampler` (10 ms frames over libwebrtc's sinc `PushResampler`), `MIX_RATE_HZ`.
- `pollis-core/src/commands/voice/mixer.rs` — `MixerShared`, `TrackSlot`, `TrackFeed`, `OutputMixer`.
- `pollis-core/src/commands/voice/limiter.rs` — `SoftLimiter`.
- `pollis-core/src/commands/voice/levels.rs` — `BandAnalyzer`, `frame_peak`, `LevelsHub`.
- `pollis-core/src/commands/voice/playback.rs` — drain tasks, `ensure_playback`, `register_remote_track` / `unregister_remote_track`.
- `pollis-core/src/commands/voice/lifecycle.rs` — `join_voice_channel` / `set_voice_audio_processing` / `set_voice_input_device` / `set_voice_output_device`, the gate.
- `pollis-core/src/commands/voice_e2ee.rs` — derives the per-room shared symmetric key from the channel's MLS exporter secret (`MlsGroup::export_secret("pollis/voice/v1", epoch, 32)`), builds the `livekit::e2ee::E2eeOptions` passed into `Room::connect`, and on MLS epoch advance installs the new key in the epoch's ring slot and re-points every local sender `FrameCryptor` at it.
- `pollis-core/src/commands/voice_key_ring.rs` — epoch → key-ring slot arithmetic (`voice_key_index`, `slot_reused_since`, `VOICE_KEY_RING_SIZE`, `VOICE_KEY_SLOT_GRACE_SECS`). Outside the media gate so it is unit-tested headless.
- `frontend/src/voice/audioLevels.ts` — the renderer side of `AudioBands` (`pushBatch`).
- `frontend/src/hooks/queries/usePreferences.ts` — `ApmConfig`, `preferencesToApmConfig`, `APM_DEFAULTS`.
- `frontend/src/pages/VoiceSettingsPage.tsx` — UI surface (mic boost slider, AGC switch + target slider, NS dropdown, AEC switch, Click Suppression switch). Mid-call changes push via `set_voice_audio_processing`.

Unit tests (`cargo test -p pollis-core --lib voice`) cover the mixer (pre-roll,
summing, limiter, concealment, drift, deafen, restart trim, 44.1 kHz devices,
ring drop counting, per-user gain), the limiter (bit-exact unity below the knee,
full-scale ceiling, monotonic knee, release), the resampler (passthrough, 44.1→48,
48→16, f32 path), `frame_peak` with `i16::MIN`, and the levels hub (coalescing,
rounding, silence suppression, removal).

## End-to-end encryption

Voice frames are E2EE between current MLS members of the channel/DM. LiveKit's
SFU forwards ciphertext; no server (LiveKit, Pollis, or otherwise) can decrypt.

- Encryption is per-frame **post-Opus / pre-SRTP**, performed by libwebrtc's
  native `FrameCryptor` (AES-GCM). Opus compresses normal PCM; the cryptor
  acts on the encoded RTP payload, so audio quality and bandwidth are
  unchanged. RTP headers stay readable so the SFU can still route.
- Key derivation: `MlsGroup::export_secret("pollis/voice/v1", epoch.to_be_bytes(), 32)`
  on the channel's MLS group (the same group its messages use). Both peers
  derive the same 32-byte key because both have the same exporter secret at
  the same epoch.
- Channel → MLS-group resolution mirrors `messages::send_message`: a row in
  `channels` means a group channel (use `channels.group_id`); no row means a
  DM whose `conversation_id` IS the MLS group id.
- Key ring: libwebrtc keeps a 16-slot key ring per participant
  (`KeyProviderOptions::key_ring_size`, passed explicitly as
  `voice_key_ring::VOICE_KEY_RING_SIZE`). A sender encrypts with the slot its
  `FrameCryptor::key_index` points at and writes that index into the frame
  trailer; a receiver decrypts with whatever it holds in the slot the trailer
  names. `set_shared_key(key, idx)` only *stores* a key — it never moves a
  cryptor, and libwebrtc rejects any `idx >= 16` (the SDK wrapper drops the
  `bool`, so `voice_e2ee::install_shared_key` reads the slot back to notice).
- Key index = `voice_key_ring::voice_key_index(epoch)` = `epoch % 16`, always
  inside the ring. Peers stay in sync because both compute the slot from the
  same epoch; consecutive epochs occupy different slots so in-flight frames
  from the previous epoch still decrypt. (The earlier `epoch & 0x7FFFFFFF`
  derivation walked off the ring at epoch 16 and the install was silently
  rejected.)
- Join: `build_e2ee_options(key, idx)` installs the join key in slot `idx`
  (and scrubs the slot-0 seed `with_shared_key` hardcodes when `idx != 0`);
  `apply_sender_key_index` points the mic's cryptor at `idx` right after
  publish. Every later local publish (screen share, camera) fires
  `RoomEvent::LocalTrackPublished`, on which `sync_local_sender_key_index`
  re-points the new cryptor — libwebrtc creates each one on slot 0.
- Rotation: when `mls::process_pending_commits_inner` applies a commit (the
  epoch advances), `voice_e2ee::on_mls_epoch_changed` re-derives the key,
  installs it at the new epoch's slot, and calls `FrameCryptor::set_key_index`
  on every LOCAL sender cryptor of the room (receivers follow the trailer).
  No reconnect. The previous slot keeps the old key for
  `VOICE_KEY_SLOT_GRACE_SECS` (10 s) so late frames and peers still applying
  the commit decrypt, then is overwritten with random bytes so an ex-member
  holding the old key cannot keep injecting frames under the old index.
  Event-driven (no polling; the scrub is a one-shot delay per rotation).
- Residual: two live epochs exactly 16 apart share a slot, so 16 commits
  inside one 10 s grace window retire the older key early — a few dropped
  frames, never plaintext exposure. `VoiceState.e2ee_key_index` records the
  live slot; `voice_key_ring.rs` (compiled on every target) holds the slot
  arithmetic and its unit tests.
- Always on. There is no opt-out; the per-frame overhead is microseconds and
  "sometimes-on" is a footgun.
- Compatibility: defaults match `livekit-client` JS (`LKFrameEncryptionKey`
  salt, PBKDF2, 16-key ring), so a future JS or mobile peer can interop if it
  derives its key the same way.

## Sample-rate model

Everything between the two cpal callbacks is 48 kHz: APM, RNNoise, the mix, the
AEC render reference, the frames handed to and received from LiveKit.

- The mic is opened at 48 kHz when the device offers it (no resampling), else at
  its native rate (Bluetooth SCO 16 kHz, a 44.1 kHz-only card) and the DSP thread
  resamples each 10 ms frame to 48 kHz through libwebrtc's `PushResampler`.
- The speaker is opened at 48 kHz when offered, else at its native rate and the
  output callback resamples the mix after the AEC tap.
- Consequences: a 16 kHz headset no longer makes every remote stream decode at
  16 kHz; a 44.1 kHz-only device gets APM; the AEC render reference is always
  sample-aligned with the capture side; the speaker-rate ≠ APM-rate "tap disabled"
  path and the "RNNoise needs 48 kHz" path no longer exist.
- Rates that are not a multiple of 100 Hz (none in practice) would get a
  truncated frame length and a ≤0.3 % pitch error; `ApmStage::new` still refuses
  anything but 8/16/32/48 kHz as a guard, which the fixed rate always satisfies.

## Frame sizes

Every stage works on 10 ms frames: 480 samples at 48 kHz (`resampler::MIX_FRAME_SAMPLES`).
At the device edges a 10 ms frame is `rate / 100` samples (`resampler::frame_samples`).
APM panics on size mismatches, so both capture and render paths hit the exact size.

## Stream delay / AEC

We run AEC3 in `EchoCanceller::Full { stream_delay_ms: None }` — APM3's internal
delay estimator is good enough on typical desktop sound stacks. The estimate is now
stable because the render reference is taken in the output callback (what is
about to play, not what a timer thought should play) and because capture keeps
running through mute, so the estimator never restarts on unmute.

`Processor::set_output_will_be_muted` is set whenever the gate is closed
(mute, deafen, PTT idle) so AGC / AEC don't adapt to the gate.

## Configuration surface

`ApmConfig` (Rust ↔ wire JSON, no rename):

| Field               | Type          | Default  | Notes |
|---------------------|---------------|----------|-------|
| `mic_boost_db`      | u8            | `0`      | Pre-AGC pre-amplifier; UI exposes 0..=20 dB. 0 = off. |
| `agc_enabled`       | bool          | `true`   | mirrors the existing `auto_gain_control` preference |
| `agc_target_dbfs`   | u8            | `6`      | AGC2 `headroom_db`; UI exposes 3..=15 (lower = louder) |
| `ns_level`          | enum (string) | `"high"` | `"off" \| "low" \| "moderate" \| "high"` |
| `aec_enabled`       | bool          | `true`   | |
| `click_suppression` | bool          | `false`  | Enables RNNoise upstream of APM. |

Per-user persistence is via `usePreferences` keys `auto_gain_control` / `agc_target_dbfs` / `noise_suppression_level` / `echo_cancellation`. `preferencesToApmConfig(prefs)` projects them into the wire shape.

Changing any of these mid-call invokes `set_voice_audio_processing`, which calls `ApmStage::set_config`. Internal echo / noise / AGC state is preserved across config changes — only the changed submodule re-initialises.

## Build dependencies

`webrtc-audio-processing` is included with the `bundled` feature. The system pkg-config branch wants `webrtc-audio-processing-2 ≥ 2.1` which most distros don't ship yet, and we'd rather every platform build from the same source.

The vendored build runs meson + ninja on the C++ source. CI installs:

- **Linux** (`apt`): `cmake clang meson ninja-build` (in addition to existing GTK/audio deps).
- **macOS** (`brew`): `meson ninja` (clang/cmake come with Xcode CLT).
- **Windows** (`choco`): `meson ninja` — installed for consistency, though the vendored APM is not actually built into the Windows binary (see the no-op note above).

For local Linux dev: `pacman -S meson ninja clang cmake` (Arch) or the equivalent for your distro.

**Linux needs clang ≥ 21 for webrtc-sys (#1303).** Since livekit 0.9, webrtc-sys compiles its C++ shims with `clang++` against the hermetic libc++ baked into libwebrtc.a, and that libc++ refuses a clang older than the one it was built with (21 today — webrtc-sys reads the floor from the artifact, so a libwebrtc bump can raise it; GCC is rejected outright). Ubuntu's apt `clang` is 14 (22.04) / 18 (24.04), so every Linux CI job that builds the media stack runs `scripts/install-clang.sh`, which adds the LLVM apt repo and puts a `clang`/`clang++` shim first on PATH (`$GITHUB_PATH`, never `$GITHUB_ENV`). Arch/Fedora's distro clang is already new enough; on Ubuntu run the script locally. `.cargo/config.toml` no longer forces `-std=c++17` in `CXXFLAGS` — it overrode webrtc-sys's `-std=c++20`, which the new headers need.

`rtrb` (the SPSC rings) is a direct dependency now; it was already in the tree via libwebrtc.

## Where APM does *not* run

- **Voice Settings test harness** (`commands/voice_test.rs`). The mic test, record/playback, and tone playback all bypass APM intentionally: users want to verify the device is delivering audio at all, not what it sounds like after processing. The level meter shows the raw pre-APM signal. (The harness shares the cpal openers, so it gets the same 10 ms device buffers.)
- **Speaker monitor loopback** (the "Hear myself" toggle). Same reason — diagnostic only.
- **Windows**, entirely — see the note at the top.

## Known gaps

- No per-host calibration of `stream_delay_ms`; we let APM3 estimate. Worth profiling per-platform before tuning.
- **No APM on Windows at all** (no MSVC support upstream in `webrtc-audio-processing-sys`). The largest of these gaps by far: it is a whole platform running unprocessed mic audio, not an edge case.
- The mic DSP thread runs at normal priority; on a loaded machine the 400 ms ring and the backlog trim cover a stall, at the cost of a dropped stretch. Elevating it (rtkit / `SCHED_FIFO`) is the next step if that shows up in the drop counters logged every 30 s.
- Drift control drops whole 10 ms frames (preferring quiet ones) rather than time-stretching; at typical ppm-level drift that is one frame every tens of seconds.
- Discord uses a heavier proprietary denoiser (Krisp) for higher quality; an open-source upgrade path is DeepFilterNet, which isn't published to crates.io yet (would require a git dep + ~5 MB model).

---
_Back to [index.md](./index.md)_
