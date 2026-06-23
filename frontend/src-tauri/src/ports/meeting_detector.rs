use std::time::Instant;

/// A single window that matches the Google Meet title pattern.
#[derive(Clone, Debug, PartialEq)]
pub struct MeetWindow {
    pub hwnd_id: usize,
    pub pid: u32,
    pub title: String,
}

/// Observation snapshot returned by the detector on each poll.
#[derive(Clone, Debug, PartialEq)]
pub struct DetectorObservation {
    /// All top-level windows whose titles match the Meet regex owned by a browser process.
    pub meet_windows: Vec<MeetWindow>,
    /// Entry signal: browser has an established TCP connection to a Google media/signalling IP
    /// AND an active WASAPI capture session (conjunction, D2). Used by the Idle → InCall
    /// transition. Not used for exit detection — see `has_browser_capture_session`.
    /// Note: for active TURN relay connections the adapter sets this `true` without
    /// consulting WASAPI — `bc` is only required as part of the conjunction in the UDP/join
    /// phase. The invariant "mc implies bc" does not hold on the TURN path.
    pub has_meet_connection: bool,
    /// Exit signal: a browser process holds an `AudioSessionStateActive` WASAPI capture
    /// session (D2 asymmetric). Stays true throughout a Meet call; drops within ~1-2 s of
    /// "Leave call". Used by the InCall → Idle transition independently of TCP state, so
    /// 90s+ TCP drops during an active UDP call do not trigger a false meeting-ended.
    pub has_browser_capture_session: bool,
    /// When the current connection was first seen. `None` if no connection is present.
    /// Set to `detector_start_time` when a connection was already present at first poll
    /// so the state machine can enforce conservative app-start behaviour (D15).
    pub connection_first_seen_at: Option<Instant>,
    /// D10: pre-resolved, stripped meeting title from the adapter (foreground window →
    /// recent focus history → first enumerated window → timestamp fallback).
    /// Empty string when `meet_windows` is empty.
    pub default_title: String,
    /// Set to `true` when a TURN relay was established for this call and has just dropped
    /// (`turn_established = true && !turn`). Enables fast 4 s debounce for TCP TURN exit
    /// so that TURN calls restore their original ~5 s exit latency instead of the 15 s
    /// debounce required for UDP calls (which have ~10 s WASAPI transients).
    /// Resets to `false` when TURN returns (transient blip), allowing the debounce timer
    /// to be cleared. `false` on all other paths.
    pub is_turn_exit: bool,
    /// Adaptive UDP-exit discriminator: `true` only while no `has_browser_capture_session`
    /// drop (a `true → false` transition) has been observed during the current call. The
    /// adapter latches it to `false` on the first such drop (the setup proved itself
    /// transient-prone) and resets it to `false` (conservative) in `notify_exit()`. The
    /// pure `step_detector` selects the UDP debounce from this flag: 4 s when `true`
    /// (stable-mic common case), 15 s when `false` (transient-prone). Ignored on the TURN
    /// path (`is_turn_exit` gates there). Mirrors the `is_turn_exit` plumbing: adapter-set
    /// bool consumed by the pure use case, no trait-method change.
    pub stable_capture: bool,
}

impl Default for DetectorObservation {
    fn default() -> Self {
        Self {
            meet_windows: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
        }
    }
}

/// Port that the platform adapter must implement.
pub trait MeetingDetectorPort {
    fn current_state(&mut self) -> DetectorObservation;
    /// Called by the use case immediately after a `MeetingEnded` event is emitted.
    /// Adapters that maintain per-call sticky state (e.g. `turn_established`) reset
    /// it here so back-to-back calls are detectable. Default is a no-op so existing
    /// implementations compile unchanged.
    fn notify_exit(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector_observation_derives_clone_debug_partialeq() {
        let obs = DetectorObservation {
            meet_windows: vec![MeetWindow {
                hwnd_id: 1,
                pid: 42,
                title: "Meet - Weekly sync".to_string(),
            }],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: None,
            default_title: "Weekly sync".to_string(),
            is_turn_exit: false,
            stable_capture: false,
        };
        let cloned = obs.clone();
        assert_eq!(obs, cloned);
        // Debug formatting must not panic
        let _ = format!("{:?}", obs);
    }
}
