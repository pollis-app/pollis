//! Shared screen-share audio must never enter the microphone pipeline (#884).
//!
//! Issue #884 names this exact bug before any code was written for it:
//!
//! > feeding shared system audio into the echo canceller as if it were the
//! > local mic would be a real bug.
//!
//! It would be, and it would be a subtle one. The mic path and the shared
//! audio path are both "10 ms mono i16 at 48 kHz" — structurally
//! interchangeable — so a wrong wire compiles, runs, and produces sound. What
//! it produces is a microphone track with a shared video mixed into it, at
//! full scale, with the APM's AGC and noise suppression fighting music they
//! were tuned to remove. Nobody would call that a crash; everyone on the call
//! would hear it.
//!
//! There are two distinct claims to keep true, and this file encodes both:
//!
//!  1. **Capture separation.** `voice_apm::run_capture` — the call that
//!     mutates a frame on its way to the *microphone* track — is only ever
//!     reached from the voice pipeline and from the shared-audio path's own
//!     dedicated, separately-constructed APM. The shared-audio publish path
//!     in `screenshare` must never hand a frame to the voice session's APM
//!     handle.
//!
//!  2. **Render-reference direction.** The voice mixer *may* hand its output
//!     to the shared-audio echo canceller — that is the whole mechanism by
//!     which Linux stops re-publishing the call (`screenshare::self_echo`).
//!     The reverse must never happen: shared audio must not be analysed as a
//!     render reference for the microphone's AEC, which would make the mic's
//!     echo canceller try to subtract a shared video from someone's voice.
//!
//! ## How it decides
//!
//! By reading source text, so this is a lower bound rather than a proof. It
//! catches the shape the mistake actually takes — a call to the wrong
//! processing function from the wrong module — which is the shape that is
//! easy to introduce, because both signals are the same type.

use std::path::{Path, PathBuf};

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(rel: &str) -> String {
    let path = src_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Strip `//` line comments so a doc comment *describing* a call is never
/// mistaken for the call. Every claim below is about executable code.
fn code_only(src: &str) -> String {
    src.lines()
        .map(|line| match line.find("//") {
            Some(i) => &line[..i],
            None => line,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Every `.rs` file under `src/commands/screenshare/`.
fn screenshare_sources() -> Vec<(String, String)> {
    let dir = src_root().join("commands").join("screenshare");
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).expect("screenshare dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .expect("file name")
            .to_string();
        out.push((name, std::fs::read_to_string(&path).expect("read")));
    }
    assert!(
        out.len() > 5,
        "expected the screenshare module to have several files; found {} — \
         has the module moved? This test would silently pass over nothing.",
        out.len()
    );
    out
}

/// Claim 1: nothing in `screenshare` reaches for the *voice session's* APM.
///
/// The shared-audio path is allowed to run an APM — it runs its own, built in
/// `self_echo::SelfEchoCanceller::new` and reachable only through that type.
/// What it must never do is pull `state.voice`'s `apm` handle and push a
/// frame through it: that handle belongs to the microphone, and a frame sent
/// through it is a frame mixed into the mic track.
#[test]
fn screenshare_never_touches_the_voice_sessions_apm() {
    for (name, src) in screenshare_sources() {
        let code = code_only(&src);
        assert!(
            !code.contains("voice.apm"),
            "screenshare/{name} reads the voice session's APM handle. That \
             handle processes the MICROPHONE; a shared-audio frame pushed \
             through it lands in the mic track. Shared audio gets its own \
             processor via self_echo::SelfEchoCanceller."
        );
        assert!(
            !code.contains(".apm.as_ref()") && !code.contains(".apm.clone()"),
            "screenshare/{name} clones a voice APM handle — see above."
        );
    }
}

/// Claim 1, the other direction: only `self_echo` runs an APM capture pass
/// inside `screenshare`.
///
/// `run_capture` is the mutating call. Confining it to one file means there
/// is exactly one place to read to know what happens to shared audio, and a
/// new call site anywhere else in the module trips this immediately.
#[test]
fn only_self_echo_runs_an_apm_capture_pass_over_shared_audio() {
    for (name, src) in screenshare_sources() {
        if name == "self_echo.rs" {
            continue;
        }
        let code = code_only(&src);
        // Matched with the open paren so `run_capture_thread` — the WGC
        // *video* thread in start_windows.rs — is not read as an APM call.
        assert!(
            !code.contains("run_capture("),
            "screenshare/{name} calls voice_apm::run_capture directly. All \
             shared-audio processing belongs in self_echo.rs, which owns a \
             dedicated AEC-only processor; a stray capture pass elsewhere \
             would silently apply speech-tuned NS/AGC to music."
        );
    }
}

/// The confinement above is only meaningful if `self_echo` really is the
/// place that does it. A test that asserts an absence everywhere can be
/// satisfied by the feature not existing at all.
#[test]
fn self_echo_is_where_the_shared_audio_capture_pass_actually_lives() {
    let code = code_only(&read("commands/screenshare/self_echo.rs"));
    assert!(
        code.contains("run_capture("),
        "self_echo.rs no longer runs an APM capture pass. Either the \
         self-echo canceller was removed — in which case Linux is \
         re-publishing the call to everyone on it — or it moved, and the \
         confinement tests above are now guarding nothing."
    );
}

/// Claim 2: shared audio is never fed to an AEC as a render reference from
/// inside `screenshare`'s publish path.
///
/// `self_echo::analyze_render` exists and is called — by the *voice mixer*,
/// handing over the signal about to hit the speaker. That is the sanctioned
/// direction. The capture side of screenshare must not call it, which would
/// mean telling an echo canceller that a shared video is what the speakers
/// are playing.
#[test]
fn shared_audio_is_never_used_as_a_render_reference() {
    for (name, src) in screenshare_sources() {
        if name == "self_echo.rs" {
            continue;
        }
        let code = code_only(&src);
        assert!(
            !code.contains("analyze_render"),
            "screenshare/{name} calls analyze_render. The render reference \
             must flow voice-mixer -> self_echo, never captured-audio -> \
             any APM: the reference is 'what the speaker is playing', and \
             shared audio is what we are recording, not what we are \
             playing."
        );
    }
}

/// The voice mixer is the one sanctioned caller, and it must stay one.
#[test]
fn the_voice_mixer_is_the_only_source_of_the_self_echo_render_reference() {
    let playback = code_only(&read("commands/voice/playback.rs"));
    assert!(
        playback.contains("self_echo::analyze_render"),
        "the voice mixer no longer feeds the shared-audio echo canceller. \
         Without that reference the canceller has nothing to subtract, and \
         a Linux screen share with sound re-publishes every participant's \
         voice back to them a few hundred milliseconds late."
    );
}

/// The microphone's own capture loop must not learn about shared audio.
///
/// `voice/lifecycle.rs` owns the mic frame task: cpal -> denoiser -> APM ->
/// `capture_frame` on the mic track. Nothing in that file should reference
/// the shared-audio modules at all. A reference here is the most direct form
/// the #884 bug could take.
#[test]
fn the_microphone_capture_path_knows_nothing_about_shared_audio() {
    let code = code_only(&read("commands/voice/lifecycle.rs"));
    for banned in [
        "screenshare::audio",
        "SharedAudioResampler",
        "SelfEchoCanceller",
        "publish_shared_audio_track",
    ] {
        assert!(
            !code.contains(banned),
            "voice/lifecycle.rs references `{banned}`. The mic capture path \
             must stay unaware of shared audio — the two signals have the \
             same shape (10 ms mono i16 @ 48 kHz), so a wrong wire here \
             compiles and runs, and only announces itself as a shared video \
             audible inside someone's microphone track."
        );
    }
}

/// A shared-audio failure must never end the video share.
///
/// The helpers report an audio-only problem through the same `Error` message
/// channel they use for fatal capture failures, distinguished by an `audio:`
/// prefix. If the parent ever stops making that distinction, a laptop with no
/// output device stops being able to screen-share at all.
#[test]
fn audio_failures_are_classified_separately_from_fatal_capture_failures() {
    let audio = code_only(&read("commands/screenshare/audio.rs"));
    assert!(
        audio.contains("fn audio_error_message"),
        "the `audio:`-prefix classifier is gone. Without it, an audio-only \
         failure is indistinguishable from a capture failure and would tear \
         down a working video share."
    );
    let start_unix = code_only(&read("commands/screenshare/start_unix.rs"));
    assert!(
        start_unix.contains("audio_error_message"),
        "start_unix.rs no longer classifies helper errors before treating \
         them as fatal — an `audio:` error would now end the share."
    );
}
