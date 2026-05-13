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
    /// Whether the browser process currently has an active connection to a Google media IP.
    pub has_meet_connection: bool,
    /// When the current connection was first seen. `None` if no connection is present.
    /// Set to `detector_start_time` when a connection was already present at first poll
    /// so the state machine can enforce conservative app-start behaviour (D15).
    pub connection_first_seen_at: Option<Instant>,
    /// D10: pre-resolved, stripped meeting title from the adapter (foreground window →
    /// recent focus history → first enumerated window → timestamp fallback).
    /// Empty string when `meet_windows` is empty.
    pub default_title: String,
}

impl Default for DetectorObservation {
    fn default() -> Self {
        Self {
            meet_windows: vec![],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        }
    }
}

/// Port that the platform adapter must implement.
pub trait MeetingDetectorPort {
    fn current_state(&mut self) -> DetectorObservation;
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
                title: "Weekly sync - Google Meet".to_string(),
            }],
            has_meet_connection: true,
            connection_first_seen_at: None,
            default_title: "Weekly sync".to_string(),
        };
        let cloned = obs.clone();
        assert_eq!(obs, cloned);
        // Debug formatting must not panic
        let _ = format!("{:?}", obs);
    }
}
