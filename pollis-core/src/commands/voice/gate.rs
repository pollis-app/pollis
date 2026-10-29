//! Local transmit / listen gate for a voice session (#849).
//!
//! One pure state machine (no audio, LiveKit or I/O) for "is my mic hot" and
//! "can I hear the room". `lifecycle.rs` mirrors its derived booleans onto
//! atomics for the audio hot paths.
//!
//! Invariant: `deafened ⇒ self_muted` (the Discord model), enforced by private
//! fields. `muted_before_deafen` is what makes undeafen non-destructive.

use serde::{Deserialize, Serialize};

/// How the microphone is gated while unmuted.
///
/// Mirrored as `VoiceInputMode` in `frontend/src/types/voice-state.ts`; keep
/// the wire strings in sync.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VoiceInputMode {
    /// The mic is open whenever you are not muted.
    #[default]
    VoiceActivity,
    /// The mic is open only while the push-to-talk key is held.
    PushToTalk,
}

/// Snapshot returned by every gate-mutating command, so the UI never
/// re-derives a transition (e.g. unmuting while deafened also undeafens).
/// Mirrored as `VoiceGateState` in `frontend/src/types/voice-state.ts`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceGateState {
    pub mode: VoiceInputMode,
    /// The user's explicit mute. Deafen forces this on.
    pub self_muted: bool,
    /// Incoming audio is silenced (and, by the invariant, so is the mic).
    pub deafened: bool,
    /// The push-to-talk key is down (meaningful only in `PushToTalk`).
    pub ptt_held: bool,
    /// Derived: captured mic frames are being published right now.
    pub transmitting: bool,
}

/// The push-to-talk / mute / deafen state machine.
///
/// Fields are private and every transition is a method, which is what
/// enforces the invariant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransmitGate {
    mode: VoiceInputMode,
    self_muted: bool,
    deafened: bool,
    ptt_held: bool,
    /// Mute state to restore when `deafened` clears; `false` otherwise.
    muted_before_deafen: bool,
}

impl Default for TransmitGate {
    fn default() -> Self {
        Self::new()
    }
}

impl TransmitGate {
    pub const fn new() -> Self {
        Self {
            mode: VoiceInputMode::VoiceActivity,
            self_muted: false,
            deafened: false,
            ptt_held: false,
            muted_before_deafen: false,
        }
    }

    pub fn mode(&self) -> VoiceInputMode {
        self.mode
    }

    pub fn self_muted(&self) -> bool {
        self.self_muted
    }

    pub fn deafened(&self) -> bool {
        self.deafened
    }

    pub fn ptt_held(&self) -> bool {
        self.ptt_held
    }

    /// Should the mic frames just captured be published? Deafen needs no arm:
    /// the invariant already forced `self_muted`.
    pub fn transmitting(&self) -> bool {
        if self.self_muted {
            return false;
        }
        match self.mode {
            VoiceInputMode::VoiceActivity => true,
            VoiceInputMode::PushToTalk => self.ptt_held,
        }
    }

    /// Push-to-talk armed and idle: not transmitting, but not muted either.
    /// The UI shows it distinctly from a mute (#849).
    pub fn ptt_idle(&self) -> bool {
        self.mode == VoiceInputMode::PushToTalk && !self.self_muted && !self.ptt_held
    }

    pub fn snapshot(&self) -> VoiceGateState {
        VoiceGateState {
            mode: self.mode,
            self_muted: self.self_muted,
            deafened: self.deafened,
            ptt_held: self.ptt_held,
            transmitting: self.transmitting(),
        }
    }

    /// Switch input mode, always dropping the push-to-talk latch so PTT starts
    /// idle and no stale latch lingers.
    pub fn set_mode(&mut self, mode: VoiceInputMode) {
        self.mode = mode;
        self.ptt_held = false;
    }

    /// Set the explicit mute. Unmuting while deafened also undeafens: the
    /// invariant forbids the alternative, and honouring the click means letting
    /// go of deafen (as Discord does).
    pub fn set_self_muted(&mut self, muted: bool) {
        if !muted && self.deafened {
            self.deafened = false;
            self.muted_before_deafen = false;
        }
        self.self_muted = muted;
        // Muting while deafened re-arms the restore slot.
        if self.deafened {
            self.muted_before_deafen = muted;
        }
    }

    pub fn toggle_mute(&mut self) {
        self.set_self_muted(!self.self_muted);
    }

    /// Set self-deafen: remember the displaced mute state and force mute on;
    /// undeafen restores exactly that, never blindly unmuting.
    pub fn set_deafened(&mut self, deafened: bool) {
        if deafened == self.deafened {
            return;
        }
        if deafened {
            self.muted_before_deafen = self.self_muted;
            self.deafened = true;
            self.self_muted = true;
            // Release the latch rather than let it decide the restored state.
            self.ptt_held = false;
        } else {
            self.deafened = false;
            self.self_muted = self.muted_before_deafen;
            self.muted_before_deafen = false;
        }
    }

    pub fn toggle_deafen(&mut self) {
        self.set_deafened(!self.deafened);
    }

    /// Push-to-talk key down (`true`) / up (`false`).
    pub fn set_ptt_held(&mut self, held: bool) {
        self.ptt_held = held;
    }

    /// The window lost focus (or the renderer is tearing down). Alt-tab with
    /// the key down delivers no keyup, so focus loss always drops the latch:
    /// at worst a clipped word, never a hidden live mic.
    pub fn on_focus_lost(&mut self) {
        self.ptt_held = false;
    }

    /// Reset per-session state at join. `mode` persists: it is a preference,
    /// not session state.
    pub fn reset_for_join(&mut self) {
        self.self_muted = false;
        self.deafened = false;
        self.ptt_held = false;
        self.muted_before_deafen = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_activity_transmits_until_muted() {
        let mut g = TransmitGate::new();
        assert_eq!(g.mode(), VoiceInputMode::VoiceActivity);
        assert!(g.transmitting());

        g.toggle_mute();
        assert!(g.self_muted());
        assert!(!g.transmitting());

        g.toggle_mute();
        assert!(g.transmitting());
    }

    #[test]
    fn voice_activity_ignores_a_stray_ptt_latch() {
        let mut g = TransmitGate::new();
        g.set_ptt_held(true);
        assert!(g.transmitting());
        g.set_ptt_held(false);
        assert!(g.transmitting());
    }

    #[test]
    fn push_to_talk_starts_idle_and_transmits_only_while_held() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        assert!(!g.transmitting(), "PTT must not open the mic on mode entry");
        assert!(g.ptt_idle());
        assert!(!g.self_muted(), "PTT idle is not a mute");

        g.set_ptt_held(true);
        assert!(g.transmitting());
        assert!(!g.ptt_idle());

        g.set_ptt_held(false);
        assert!(!g.transmitting(), "releasing the key must return to silence");
        assert!(g.ptt_idle());
    }

    #[test]
    fn push_to_talk_is_overridden_by_an_explicit_mute() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_self_muted(true);

        g.set_ptt_held(true);
        assert!(!g.transmitting(), "holding PTT must not defeat an explicit mute");
        assert!(!g.ptt_idle(), "an explicit mute reads as muted, not PTT-idle");
    }

    #[test]
    fn leaving_push_to_talk_mode_drops_the_latch() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_ptt_held(true);
        assert!(g.transmitting());

        g.set_mode(VoiceInputMode::VoiceActivity);
        assert!(!g.ptt_held(), "the latch must not survive a mode change");
        assert!(g.transmitting());
    }

    #[test]
    fn re_entering_push_to_talk_does_not_inherit_a_latch() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_ptt_held(true);
        g.set_mode(VoiceInputMode::VoiceActivity);
        g.set_mode(VoiceInputMode::PushToTalk);
        assert!(!g.transmitting());
    }

    #[test]
    fn focus_loss_while_ptt_held_stops_transmitting() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_ptt_held(true);
        assert!(g.transmitting(), "precondition: mic is hot");

        g.on_focus_lost();

        assert!(!g.ptt_held());
        assert!(!g.transmitting(), "focus loss must close the mic");
        assert!(g.ptt_idle(), "and land back in the armed-idle state");
    }

    #[test]
    fn focus_loss_is_idempotent_and_never_unmutes() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_self_muted(true);
        g.set_ptt_held(true);

        g.on_focus_lost();
        g.on_focus_lost();

        assert!(g.self_muted(), "focus loss must not clear an explicit mute");
        assert!(!g.transmitting());
    }

    #[test]
    fn focus_loss_in_voice_activity_mode_changes_nothing() {
        let mut g = TransmitGate::new();
        assert!(g.transmitting());
        g.on_focus_lost();
        assert!(
            g.transmitting(),
            "voice-activity users must not be silenced by alt-tab"
        );
    }

    #[test]
    fn a_keyup_that_never_arrives_cannot_strand_the_mic() {
        // Keydown, blur, then the stray keyup arrives later.
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_ptt_held(true);
        g.on_focus_lost();
        g.set_ptt_held(false);
        assert!(!g.transmitting());
    }

    #[test]
    fn deafen_implies_self_mute() {
        let mut g = TransmitGate::new();
        assert!(!g.self_muted());

        g.toggle_deafen();

        assert!(g.deafened());
        assert!(g.self_muted(), "deafen must imply self-mute");
        assert!(!g.transmitting());
    }

    #[test]
    fn undeafen_restores_the_unmuted_state_it_displaced() {
        let mut g = TransmitGate::new();
        g.set_deafened(true);
        assert!(g.self_muted());

        g.set_deafened(false);

        assert!(!g.deafened());
        assert!(!g.self_muted(), "undeafen must restore the prior UNMUTED state");
        assert!(g.transmitting());
    }

    #[test]
    fn undeafen_restores_the_muted_state_it_displaced() {
        let mut g = TransmitGate::new();
        g.set_self_muted(true);
        g.set_deafened(true);
        assert!(g.self_muted());

        g.set_deafened(false);

        assert!(!g.deafened());
        assert!(
            g.self_muted(),
            "undeafen must NOT blindly unmute someone who was already muted"
        );
        assert!(!g.transmitting());
    }

    #[test]
    fn muting_while_deafened_updates_what_undeafen_restores() {
        let mut g = TransmitGate::new();
        g.set_deafened(true);
        assert!(!g.transmitting());

        // User mutes explicitly while deafened; undeafen should honour it.
        g.set_self_muted(true);
        g.set_deafened(false);

        assert!(g.self_muted());
        assert!(!g.transmitting());
    }

    #[test]
    fn unmuting_while_deafened_also_undeafens() {
        let mut g = TransmitGate::new();
        g.set_deafened(true);

        g.set_self_muted(false);

        assert!(!g.deafened(), "the invariant forbids deafened && !self_muted");
        assert!(!g.self_muted());
        assert!(g.transmitting());
    }

    #[test]
    fn deafen_is_idempotent() {
        let mut g = TransmitGate::new();
        g.set_self_muted(true);
        g.set_deafened(true);
        // A repeat deafen must not overwrite the restore slot.
        g.set_deafened(true);
        g.set_deafened(false);
        assert!(g.self_muted(), "restore slot survived a redundant deafen");

        let mut h = TransmitGate::new();
        h.set_deafened(true);
        h.set_deafened(true);
        h.set_deafened(false);
        assert!(!h.self_muted());
    }

    #[test]
    fn deafen_releases_a_held_ptt_key() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_ptt_held(true);
        assert!(g.transmitting());

        g.set_deafened(true);

        assert!(!g.ptt_held());
        assert!(!g.transmitting());
    }

    #[test]
    fn undeafen_in_ptt_mode_returns_to_idle_not_hot() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_deafened(true);

        g.set_deafened(false);

        assert!(!g.self_muted(), "restored to unmuted…");
        assert!(!g.transmitting(), "…but PTT still gates the mic");
        assert!(g.ptt_idle());
    }

    #[test]
    fn deafen_survives_a_round_trip_through_ptt() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_deafened(true);
        g.set_ptt_held(true);
        assert!(!g.transmitting());
        assert!(g.deafened());
    }

    #[test]
    fn reset_for_join_clears_session_state_but_keeps_the_mode() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        g.set_deafened(true);
        g.set_ptt_held(true);

        g.reset_for_join();

        assert_eq!(
            g.mode(),
            VoiceInputMode::PushToTalk,
            "input mode is a preference, not session state"
        );
        assert!(!g.self_muted());
        assert!(!g.deafened());
        assert!(!g.ptt_held());
        assert!(!g.transmitting());

        g.set_deafened(true);
        g.set_deafened(false);
        assert!(!g.self_muted());
    }

    #[test]
    fn snapshot_reports_the_derived_transmit_state() {
        let mut g = TransmitGate::new();
        g.set_mode(VoiceInputMode::PushToTalk);
        let s = g.snapshot();
        assert_eq!(s.mode, VoiceInputMode::PushToTalk);
        assert!(!s.transmitting);
        assert!(!s.self_muted);
        assert!(!s.deafened);
        assert!(!s.ptt_held);

        g.set_ptt_held(true);
        assert!(g.snapshot().transmitting);
    }

    #[test]
    fn snapshot_serializes_mode_as_snake_case() {
        let g = TransmitGate::new();
        let json = serde_json::to_string(&g.snapshot()).unwrap();
        assert!(json.contains("\"voice_activity\""), "got {json}");

        let mut h = TransmitGate::new();
        h.set_mode(VoiceInputMode::PushToTalk);
        let json = serde_json::to_string(&h.snapshot()).unwrap();
        assert!(json.contains("\"push_to_talk\""), "got {json}");
    }

    /// The invariant, swept exhaustively over every transition sequence of
    /// length 4.
    #[test]
    // The three assertions below are each written as "never (A and B)" because
    // that is the invariant; De Morgan'ing one of them would break the set's
    // parallel shape for no gain.
    #[allow(clippy::nonminimal_bool)]
    fn invariant_holds_over_all_short_transition_sequences() {
        #[derive(Clone, Copy)]
        enum Op {
            ToggleMute,
            ToggleDeafen,
            PttDown,
            PttUp,
            ModeVa,
            ModePtt,
            FocusLost,
            Join,
        }
        const OPS: [Op; 8] = [
            Op::ToggleMute,
            Op::ToggleDeafen,
            Op::PttDown,
            Op::PttUp,
            Op::ModeVa,
            Op::ModePtt,
            Op::FocusLost,
            Op::Join,
        ];

        fn apply(g: &mut TransmitGate, op: Op) {
            match op {
                Op::ToggleMute => g.toggle_mute(),
                Op::ToggleDeafen => g.toggle_deafen(),
                Op::PttDown => g.set_ptt_held(true),
                Op::PttUp => g.set_ptt_held(false),
                Op::ModeVa => g.set_mode(VoiceInputMode::VoiceActivity),
                Op::ModePtt => g.set_mode(VoiceInputMode::PushToTalk),
                Op::FocusLost => g.on_focus_lost(),
                Op::Join => g.reset_for_join(),
            }
        }

        for a in OPS {
            for b in OPS {
                for c in OPS {
                    for d in OPS {
                        let mut g = TransmitGate::new();
                        for op in [a, b, c, d] {
                            apply(&mut g, op);
                            assert!(
                                !(g.deafened() && !g.self_muted()),
                                "invariant violated: deafened without self-mute"
                            );
                            assert!(
                                !(g.deafened() && g.transmitting()),
                                "invariant violated: transmitting while deafened"
                            );
                            assert!(
                                !(g.self_muted() && g.transmitting()),
                                "invariant violated: transmitting while self-muted"
                            );
                            assert!(
                                !(g.ptt_idle() && g.transmitting()),
                                "invariant violated: PTT-idle while transmitting"
                            );
                        }
                    }
                }
            }
        }
    }
}
