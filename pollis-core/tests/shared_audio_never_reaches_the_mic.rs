//! Shared screen-share audio must never enter the microphone pipeline (#884).
//!
//! Both signals are 10 ms mono i16 @ 48 kHz, so a wrong wire compiles and runs.
//! Reads source text: a lower bound, not a proof.

use std::path::{Path, PathBuf};

fn src_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("src")
}

fn read(rel: &str) -> String {
    let path = src_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Strip `//` line comments so a comment describing a call is not the call.
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

/// Nothing in `screenshare` reaches for the *voice session's* APM.
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

/// Only `self_echo` runs an APM capture pass inside `screenshare`.
#[test]
fn only_self_echo_runs_an_apm_capture_pass_over_shared_audio() {
    for (name, src) in screenshare_sources() {
        if name == "self_echo.rs" {
            continue;
        }
        let code = code_only(&src);
        // The paren excludes `run_capture_thread` (WGC video).
        assert!(
            !code.contains("run_capture("),
            "screenshare/{name} calls voice_apm::run_capture directly. All \
             shared-audio processing belongs in self_echo.rs, which owns a \
             dedicated AEC-only processor; a stray capture pass elsewhere \
             would silently apply speech-tuned NS/AGC to music."
        );
    }
}

/// Positive control for the confinement above.
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

/// Shared audio is never fed to an AEC as a render reference.
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

/// A shared-audio failure (`audio:`-prefixed `Error`) must never end the video share.
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
