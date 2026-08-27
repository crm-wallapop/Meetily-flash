use std::time::Instant;

/// A top-level browser-process window observed by `EnumWindows`. The title is
/// vendor-neutral raw text — the `MeetingTitleExtractorPort` decides whether it
/// matches a known conference vendor; this struct makes no claim about the title's
/// semantics.
#[derive(Clone, Debug, PartialEq)]
pub struct BrowserWindow {
    pub hwnd_id: usize,
    pub pid: u32,
    pub title: String,
}

/// Observation snapshot returned by the detector on each poll.
#[derive(Clone, Debug, PartialEq)]
pub struct DetectorObservation {
    /// All visible top-level windows owned by a browser process (no title
    /// pre-filter — vendor matching happens in the wired `MeetingTitleExtractorPort`).
    pub browser_windows: Vec<BrowserWindow>,
    /// Titles the wired `MeetingTitleExtractorPort` recognised from
    /// `browser_windows` (adapter-populated, per-window). Forwarded to the
    /// frontend as candidate recording names. Empty when no vendor match.
    pub candidate_titles: Vec<String>,
    /// Entry signal fed to the `Idle → InCall` transition. The VALUE is vendor-neutral —
    /// `has_meet_connection = signaling_active && has_browser_capture_session`, where
    /// `signaling_active` is the OR of wired `CallSignalingPort` adapters (v1: Meet's
    /// Google-CIDR check). The field NAME is retained as `has_meet_connection` for
    /// canonical-spec continuity; the rename is deferred to the second-vendor change.
    /// Not used for exit detection — see `has_browser_capture_session`. Because entry
    /// requires capture, the invariant "mc implies bc" holds.
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
    /// recent focus history → first enumerated window → timestamp fallback). Always
    /// populated — the timestamp fallback guarantees a non-empty string even when no
    /// browser window is visible.
    pub default_title: String,
    /// Adaptive UDP-exit discriminator. Decided ONCE per call, at the first
    /// `has_browser_capture_session()` `true → false` drop, from the length of the
    /// unbroken `true` run immediately preceding that drop: `true` iff that run was ≥
    /// `STABLE_CONFIDENCE_WINDOW` (~20 s), else `false`. Stored in the adapter's
    /// per-call `exit_stable_latch` and held IMMUTABLE for the rest of the call —
    /// neither a `false → true` recovery nor a later drop may change it — until
    /// `notify_exit()` resets it. The immutability is mandatory: `step_detector`
    /// recomputes the debounce every poll, so the value driving it must not flip
    /// mid-debounce (the prior recovery-based design recreated the self-heal trap of
    /// commit 693ff90). The pure `step_detector` selects the UDP debounce from this
    /// flag: 4 s when `true` (stable-mic common case), 15 s when `false`
    /// (transient-prone / short run).
    ///
    /// 2026-08-26: replaced the TURN-exit fast path (`is_turn_exit`). The TCP
    /// "TURN relay" scan never saw real Meet calls (media is UDP, invisible to
    /// the TCP table) and its CIDR ranges overlapped general Google Cloud
    /// hosting, producing false-positive detections. Exit latency for typical
    /// calls is unchanged: the stable-capture latch already grants the same 4 s
    /// debounce after ~20 s of continuous capture.
    pub stable_capture: bool,
}

impl Default for DetectorObservation {
    fn default() -> Self {
        Self {
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            stable_capture: false,
        }
    }
}

/// Port that the platform adapter must implement.
pub trait MeetingDetectorPort {
    fn current_state(&mut self) -> DetectorObservation;
    /// Called by the use case immediately after a `MeetingEnded` event is emitted.
    /// Adapters that maintain per-call sticky state (exit latches, first-seen
    /// suppression) reset it here so back-to-back calls are detectable. Default is a no-op so existing
    /// implementations compile unchanged.
    fn notify_exit(&mut self) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detector_observation_derives_clone_debug_partialeq() {
        let obs = DetectorObservation {
            browser_windows: vec![BrowserWindow {
                hwnd_id: 1,
                pid: 42,
                title: "Meet - Weekly sync".to_string(),
            }],
            candidate_titles: vec!["Weekly sync".to_string()],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: None,
            default_title: "Weekly sync".to_string(),
            stable_capture: false,
        };
        let cloned = obs.clone();
        assert_eq!(obs, cloned);
        // Debug formatting must not panic
        let _ = format!("{:?}", obs);
    }
}
