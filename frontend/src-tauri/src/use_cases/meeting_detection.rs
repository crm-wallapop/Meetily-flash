use crate::ports::meeting_detector::{DetectorObservation, MeetingDetectorPort};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DetectorSettings {
    /// How long the connection must be absent before a meeting-ended event fires.
    pub debounce_duration: Duration,
}

impl Default for DetectorSettings {
    fn default() -> Self {
        Self {
            debounce_duration: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum DetectorEvent {
    MeetingDetected {
        default_title: String,
        candidate_titles: Vec<String>,
    },
    MeetingEnded,
}

/// State of the detector state machine.
#[derive(Debug, Clone)]
pub enum DetectorState {
    Idle,
    InCall {
        /// When the connection was first observed as absent (for 10s debounce).
        connection_lost_at: Option<Instant>,
    },
}

/// Abstraction over Tauri event emission so the state machine remains testable
/// without a real Tauri runtime.
pub trait DetectorEventEmitter: Send + Sync + 'static {
    fn emit_detected(&self, default_title: String, candidate_titles: Vec<String>);
    fn emit_ended(&self);
}

// ── Pure state-machine step ────────────────────────────────────────────────

/// Advances the state machine by one observation.
///
/// Returns the next state and any events to emit. The caller (spawner) is
/// responsible for calling the emitter with the returned events.
///
/// `now` is the current instant — injected so tests can control time without
/// real sleeps.
pub fn step_detector(
    state: DetectorState,
    observation: &DetectorObservation,
    detector_start: Instant,
    now: Instant,
    suppress_signal: &AtomicBool,
    settings: &DetectorSettings,
) -> (DetectorState, Vec<DetectorEvent>) {
    match state {
        DetectorState::Idle => {
            let has_title = !observation.meet_windows.is_empty();
            let has_conn = observation.has_meet_connection;
            // Only fire for connections that appeared after the detector started (D15).
            let not_preexisting = observation
                .connection_first_seen_at
                .map(|t| t > detector_start)
                .unwrap_or(false);

            if has_title && has_conn && not_preexisting {
                let default_title = observation.default_title.clone();
                let candidate_titles = observation
                    .meet_windows
                    .iter()
                    .map(|w| w.title.clone())
                    .collect();
                let event = DetectorEvent::MeetingDetected {
                    default_title,
                    candidate_titles,
                };
                let new_state = DetectorState::InCall {
                    connection_lost_at: None,
                };
                (new_state, vec![event])
            } else {
                (DetectorState::Idle, vec![])
            }
        }

        DetectorState::InCall {
            mut connection_lost_at,
        } => {
            // consume the cancel signal so the spawner knows the frontend acknowledged it
            suppress_signal.compare_exchange(true, false, Ordering::AcqRel, Ordering::Acquire).ok();

            if observation.has_meet_connection {
                connection_lost_at = None;
                (
                    DetectorState::InCall { connection_lost_at },
                    vec![],
                )
            } else {
                let lost_at = connection_lost_at.unwrap_or(now);
                let elapsed = now.duration_since(lost_at);
                log::debug!(
                    "InCall: no connection — debounce {:.1}s / {:.1}s",
                    elapsed.as_secs_f32(),
                    settings.debounce_duration.as_secs_f32(),
                );
                if elapsed >= settings.debounce_duration {
                    (DetectorState::Idle, vec![DetectorEvent::MeetingEnded])
                } else {
                    (
                        DetectorState::InCall {
                            connection_lost_at: Some(lost_at),
                        },
                        vec![],
                    )
                }
            }
        }
    }
}

// ── Spawner ───────────────────────────────────────────────────────────────

/// Starts the detection polling loop in a Tokio task.
///
/// The caller retains the `cancel_suppress_signal`; setting it to `true` signals
/// the state machine to stop re-detecting the current call after user cancels the
/// auto-start banner.
pub fn spawn_detector<P, E>(
    mut port: P,
    emitter: E,
    poll_interval: Duration,
    settings: DetectorSettings,
    cancel_suppress_signal: Arc<AtomicBool>,
) -> JoinHandle<()>
where
    P: MeetingDetectorPort + Send + 'static,
    E: DetectorEventEmitter,
{
    tokio::spawn(async move {
        let detector_start = Instant::now();
        let mut state = DetectorState::Idle;

        loop {
            // a panicking port must not bring down the polling loop
            let observation = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                port.current_state()
            })) {
                Ok(obs) => obs,
                Err(_) => {
                    log::error!("[spawn_detector] port.current_state() panicked — skipping poll");
                    tokio::time::sleep(poll_interval).await;
                    continue;
                }
            };
            let now = Instant::now();
            let (next_state, events) = step_detector(
                state,
                &observation,
                detector_start,
                now,
                &cancel_suppress_signal,
                &settings,
            );
            state = next_state;

            for event in events {
                match event {
                    DetectorEvent::MeetingDetected {
                        default_title,
                        candidate_titles,
                    } => emitter.emit_detected(default_title, candidate_titles),
                    DetectorEvent::MeetingEnded => emitter.emit_ended(),
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    })
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::ports::meeting_detector::{DetectorObservation, MeetWindow, MeetingDetectorPort};
    use std::collections::VecDeque;
    use std::sync::Mutex;

    // ── Test doubles ──────────────────────────────────────────────────────

    /// Scriptable mock: returns observations from a VecDeque in order,
    /// repeating the last one once the queue is exhausted.
    pub struct MockMeetingDetector {
        observations: Mutex<VecDeque<DetectorObservation>>,
        fallback: DetectorObservation,
    }

    impl MockMeetingDetector {
        pub fn new(sequence: impl IntoIterator<Item = DetectorObservation>) -> Self {
            let q: VecDeque<DetectorObservation> = sequence.into_iter().collect();
            let fallback = q.back().cloned().unwrap_or_else(idle_obs);
            Self {
                observations: Mutex::new(q),
                fallback,
            }
        }
    }

    impl MeetingDetectorPort for MockMeetingDetector {
        fn current_state(&mut self) -> DetectorObservation {
            let mut q = self.observations.lock().unwrap();
            q.pop_front().unwrap_or_else(|| self.fallback.clone())
        }
    }

    pub struct MockEmitter {
        pub detected: Mutex<Vec<(String, Vec<String>)>>,
        pub ended_count: Mutex<u32>,
    }

    impl Default for MockEmitter {
        fn default() -> Self {
            Self {
                detected: Mutex::new(vec![]),
                ended_count: Mutex::new(0),
            }
        }
    }

    impl DetectorEventEmitter for MockEmitter {
        fn emit_detected(&self, default_title: String, candidate_titles: Vec<String>) {
            self.detected.lock().unwrap().push((default_title, candidate_titles));
        }
        fn emit_ended(&self) {
            *self.ended_count.lock().unwrap() += 1;
        }
    }

    /// Allow Arc<MockEmitter> as emitter so the test can hold a clone for assertions.
    impl DetectorEventEmitter for std::sync::Arc<MockEmitter> {
        fn emit_detected(&self, default_title: String, candidate_titles: Vec<String>) {
            MockEmitter::emit_detected(self, default_title, candidate_titles);
        }
        fn emit_ended(&self) {
            MockEmitter::emit_ended(self);
        }
    }

    /// Port that panics for the first `panic_until` calls, then returns `success_obs`.
    /// Used in task 4.5 to verify the spawner loop survives port panics.
    struct PanickingPort {
        call_count: u32,
        panic_until: u32,
        success_obs: DetectorObservation,
    }

    impl MeetingDetectorPort for PanickingPort {
        fn current_state(&mut self) -> DetectorObservation {
            self.call_count += 1;
            if self.call_count <= self.panic_until {
                panic!("simulated port panic #{}", self.call_count);
            }
            self.success_obs.clone()
        }
    }

    // ── Helpers ───────────────────────────────────────────────────────────

    fn meet_window(title: &str) -> MeetWindow {
        MeetWindow {
            hwnd_id: 1,
            pid: 100,
            title: title.to_string(),
        }
    }

    fn idle_obs() -> DetectorObservation {
        DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        }
    }

    /// An observation that should trigger detection: title match + fresh connection.
    fn detected_obs(title: &str, detector_start: Instant) -> DetectorObservation {
        let conn_seen = detector_start + Duration::from_millis(500); // appeared after start
        DetectorObservation {
            meet_windows: vec![meet_window(title)],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: title.to_string(),
        }
    }

    fn default_settings() -> DetectorSettings {
        DetectorSettings {
            debounce_duration: Duration::from_secs(10),
        }
    }

    fn no_suppress() -> AtomicBool {
        AtomicBool::new(false)
    }

    // ── 2.1 ───────────────────────────────────────────────────────────────
    // Idle → InCall: title match + connection + fresh → emit meeting-detected.
    #[test]
    fn test_2_1_idle_transitions_to_in_call_on_valid_observation() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(500);

        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Weekly sync - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: "Weekly sync - Google Meet".to_string(),
        };

        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs,
            start,
            Instant::now(),
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(state, DetectorState::InCall { .. }));
        assert_eq!(events.len(), 1);
        assert!(
            matches!(&events[0], DetectorEvent::MeetingDetected { default_title, .. } if default_title == "Weekly sync - Google Meet")
        );
    }

    // ── 2.2 ───────────────────────────────────────────────────────────────
    // App-start state (D15): connection was already present at detector start → no event.
    #[test]
    fn test_2_2_preexisting_connection_does_not_fire() {
        let start = Instant::now();
        // connection_first_seen_at == detector_start_time → pre-existing
        let obs = DetectorObservation {
            meet_windows: vec![meet_window("All-hands - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(start),
            default_title: String::new(),
        };

        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs,
            start,
            Instant::now(),
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(state, DetectorState::Idle));
        assert!(events.is_empty());
    }

    // ── 2.3 ───────────────────────────────────────────────────────────────
    // InCall: transient drop < debounce → no meeting-ended.
    #[test]
    fn test_2_3_transient_drop_within_debounce_no_ended_event() {
        let now = Instant::now();
        // connection lost 5 seconds ago (< 10s debounce)
        let lost_5s_ago = now - Duration::from_secs(5);

        let obs = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        };

        let state = DetectorState::InCall {
            connection_lost_at: Some(lost_5s_ago),
        };

        let (new_state, events) = step_detector(
            state,
            &obs,
            now - Duration::from_secs(60), // detector started a minute ago
            now,
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(new_state, DetectorState::InCall { .. }));
        assert!(events.is_empty());
    }

    // ── 2.4 ───────────────────────────────────────────────────────────────
    // InCall: connection absent ≥ debounce → emit meeting-ended, transition to Idle.
    #[test]
    fn test_2_4_connection_absent_beyond_debounce_fires_ended() {
        let now = Instant::now();
        // connection lost 11 seconds ago (> 10s debounce)
        let lost_11s_ago = now - Duration::from_secs(11);

        let obs = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        };

        let state = DetectorState::InCall {
            connection_lost_at: Some(lost_11s_ago),
        };

        let (new_state, events) = step_detector(
            state,
            &obs,
            now - Duration::from_secs(60),
            now,
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(new_state, DetectorState::Idle));
        assert_eq!(events, vec![DetectorEvent::MeetingEnded]);
    }

    // ── 2.5 ───────────────────────────────────────────────────────────────
    // Cancel-suppression (D16): within the same InCall session, a transient drop
    // and return does NOT re-emit meeting-detected. InCall never emits meeting-detected,
    // so this holds structurally. The suppress signal is consumed (edge-detect) to
    // prevent it from accumulating. After the debounce expires → Idle, detection
    // fires normally for a new call.
    #[test]
    fn test_2_5_cancel_suppression_prevents_re_detection_within_call() {
        let start = Instant::now();
        let suppress = AtomicBool::new(true); // frontend signalled cancel

        let obs_lost = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        };

        // Step 1: cancel signal consumed, connection just lost.
        let state = DetectorState::InCall {
            connection_lost_at: None,
        };
        let (state, events) = step_detector(state, &obs_lost, start, Instant::now(), &suppress, &default_settings());
        assert!(events.is_empty(), "no event on first loss");
        assert!(matches!(state, DetectorState::InCall { .. }));
        // signal was consumed
        assert!(!suppress.load(Ordering::Acquire), "suppress signal must be cleared after consumption");

        // Step 2: connection returns (< 10s) → still InCall, no re-emit.
        let obs_back = DetectorObservation {
            meet_windows: vec![meet_window("Sync - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(start + Duration::from_millis(500)),
            default_title: String::new(),
        };
        let (state, events) = step_detector(state, &obs_back, start, Instant::now(), &AtomicBool::new(false), &default_settings());
        assert!(events.is_empty(), "no re-emit after transient drop+return");
        assert!(matches!(state, DetectorState::InCall { .. }));

        // Step 3: connection drops for > 10s → transition to Idle.
        let now = Instant::now();
        let lost_11s_ago = now - Duration::from_secs(11);
        let state = DetectorState::InCall {
            connection_lost_at: Some(lost_11s_ago),
        };
        let obs_gone = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        };
        let (state, events) = step_detector(state, &obs_gone, start, now, &AtomicBool::new(false), &default_settings());
        assert!(matches!(state, DetectorState::Idle));
        assert_eq!(events, vec![DetectorEvent::MeetingEnded]);

        // Step 4: new connection → must re-emit.
        let conn_seen = now + Duration::from_millis(500);
        let obs_new = DetectorObservation {
            meet_windows: vec![meet_window("New call - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: "New call - Google Meet".to_string(),
        };
        let (_, events) = step_detector(state, &obs_new, start, conn_seen, &AtomicBool::new(false), &default_settings());
        assert_eq!(events.len(), 1, "new call after Idle reset must re-emit");
        assert!(matches!(&events[0], DetectorEvent::MeetingDetected { default_title, .. } if default_title == "New call - Google Meet"));
    }

    // ── 2.6 ───────────────────────────────────────────────────────────────
    // Rapid alternation within 10s does NOT emit meeting-ended.
    #[test]
    fn test_2_6_rapid_alternation_within_debounce_no_ended() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(500);

        // Start in InCall
        let state = DetectorState::InCall {
            connection_lost_at: None,
        };

        let obs_false = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        };
        let obs_true = DetectorObservation {
            meet_windows: vec![meet_window("Sprint - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
        };

        // true → false → true → false, each step < 2s apart
        let now = Instant::now();
        let (s, e) = step_detector(state, &obs_false, start, now, &no_suppress(), &default_settings());
        assert!(e.is_empty());

        let now2 = now + Duration::from_secs(1);
        let (s, e) = step_detector(s, &obs_true, start, now2, &no_suppress(), &default_settings());
        assert!(e.is_empty());

        let now3 = now2 + Duration::from_secs(1);
        let (s, e) = step_detector(s, &obs_false, start, now3, &no_suppress(), &default_settings());
        assert!(e.is_empty());

        let now4 = now3 + Duration::from_secs(1);
        let (_, e) = step_detector(s, &obs_false, start, now4, &no_suppress(), &default_settings());
        assert!(e.is_empty(), "total 3s < 10s debounce → no ended");
    }

    // ── 2.7 ───────────────────────────────────────────────────────────────
    // Title match WITHOUT has_meet_connection (Meet tab open, user not joined) → Idle.
    #[test]
    fn test_2_7_title_without_connection_stays_idle() {
        let start = Instant::now();
        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Sync - Google Meet")],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        };

        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs,
            start,
            Instant::now(),
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(state, DetectorState::Idle));
        assert!(events.is_empty());
    }

    // ── 2.8 ───────────────────────────────────────────────────────────────
    // has_meet_connection WITHOUT title match → Idle.
    #[test]
    fn test_2_8_connection_without_title_stays_idle() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(500);
        let obs = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
        };

        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs,
            start,
            Instant::now(),
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(state, DetectorState::Idle));
        assert!(events.is_empty());
    }

    // ── 4.4 ───────────────────────────────────────────────────────────────
    // D17: the Rust state machine always emits meeting-detected from Idle when
    // conditions are met. It has no knowledge of the frontend recording state.
    // The frontend useAutoDetect hook guards against double-start via isRecordingRef.
    #[test]
    fn test_4_4_state_machine_emits_regardless_of_frontend_recording_state() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(500);

        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Standup - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
        };

        // Even if frontend is "recording" (not tracked in the state machine), Rust emits.
        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs,
            start,
            Instant::now(),
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(state, DetectorState::InCall { .. }));
        assert_eq!(events.len(), 1);
        assert!(matches!(&events[0], DetectorEvent::MeetingDetected { .. }));
        // Rust emits the event; the frontend hook (useAutoDetect.ts, isRecordingRef guard)
        // is responsible for ignoring it when a recording is already in progress (D17).
    }

    // ── 4.5 ───────────────────────────────────────────────────────────────
    // A panicking port must not crash the spawner loop. After the panic is caught
    // the loop must continue polling, eventually emitting when the port recovers.
    #[tokio::test]
    async fn test_4_5_port_panic_does_not_crash_detector() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(100);
        let success_obs = DetectorObservation {
            meet_windows: vec![meet_window("Resilience - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
        };

        // Panics on first 2 calls, then succeeds.
        let port = PanickingPort {
            call_count: 0,
            panic_until: 2,
            success_obs,
        };

        let emitter = std::sync::Arc::new(MockEmitter::default());
        let emitter_for_spawn = std::sync::Arc::clone(&emitter);
        let suppress = std::sync::Arc::new(AtomicBool::new(false));

        let handle = spawn_detector(
            port,
            emitter_for_spawn,
            Duration::from_millis(5),
            DetectorSettings { debounce_duration: Duration::from_secs(10) },
            suppress,
        );

        // Wait long enough for 2 panics + 1 success (~30ms at 5ms intervals).
        tokio::time::sleep(Duration::from_millis(150)).await;
        handle.abort();

        let detected = emitter.detected.lock().unwrap();
        assert!(
            !detected.is_empty(),
            "detector should have continued polling after panics and emitted meeting-detected"
        );
    }

    // ── 4.6 ───────────────────────────────────────────────────────────────
    // Spotify-desktop false positive: browser process has a Meet-looking title
    // but `has_meet_connection=false` (Spotify is not connected to Google IPs).
    // The detector must stay Idle.
    #[test]
    fn test_4_6_spotify_fp_title_match_no_connection_stays_idle() {
        let start = Instant::now();
        let obs = DetectorObservation {
            meet_windows: vec![meet_window("All-hands - Google Meet")],
            has_meet_connection: false, // Spotify: no Google-IP connection
            connection_first_seen_at: None,
            default_title: String::new(),
        };

        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs,
            start,
            Instant::now(),
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(state, DetectorState::Idle));
        assert!(events.is_empty(), "Spotify FP: title match without connection must not fire");
    }

    // ── 4.7 ───────────────────────────────────────────────────────────────
    // Discord-PWA false positive: title matches but WebRTC is not to Google IPs
    // so the adapter returns has_meet_connection=false. Stays Idle.
    #[test]
    fn test_4_7_discord_pwa_fp_title_match_no_connection_stays_idle() {
        let start = Instant::now();
        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Team call - Google Meet")],
            has_meet_connection: false, // Discord: connection is not to Google IPs
            connection_first_seen_at: None,
            default_title: String::new(),
        };

        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs,
            start,
            Instant::now(),
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(state, DetectorState::Idle));
        assert!(events.is_empty(), "Discord PWA FP: must not fire without Google-IP connection");
    }

    // ── 4.8 ───────────────────────────────────────────────────────────────
    // App-start mid-call (D15): user is already on a Meet call when Meetily launches.
    // The first poll has connection_first_seen_at == detector_start_time.
    // The detector must NOT emit — it should only fire for connections that appeared
    // AFTER the detector started.
    #[test]
    fn test_4_8_app_start_mid_call_does_not_fire() {
        let detector_start = Instant::now();

        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Q4 review - Google Meet")],
            has_meet_connection: true,
            // connection_first_seen_at == detector_start → pre-existing (D15)
            connection_first_seen_at: Some(detector_start),
            default_title: String::new(),
        };

        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs,
            detector_start,
            Instant::now(),
            &no_suppress(),
            &default_settings(),
        );

        assert!(matches!(state, DetectorState::Idle), "pre-existing connection must not transition to InCall");
        assert!(events.is_empty(), "app-start D15: must not emit meeting-detected for pre-existing call");
    }

    // ── 4.9 ───────────────────────────────────────────────────────────────
    // Full cancel-suppression scripted sequence (D16):
    // 1. meeting-detected fires
    // 2. frontend signals cancel (suppress=true)
    // 3. connection drops 8s (< 10s debounce) → no ended, no re-detect
    // 4. connection returns → stays InCall, no re-detect (cancel-suppressed)
    // 5. connection drops 12s (> 10s debounce) → meeting-ended, transition to Idle (flag reset)
    // 6. new connection → meeting-detected fires again (flag was reset)
    #[test]
    fn test_4_9_cancel_suppression_full_scripted_sequence() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(500);

        // ── Step 1: Idle → InCall, meeting-detected fires ──────────────────
        let obs_detected = DetectorObservation {
            meet_windows: vec![meet_window("Sprint planning - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
        };
        let (state, events) = step_detector(
            DetectorState::Idle,
            &obs_detected,
            start,
            conn_seen,
            &no_suppress(),
            &default_settings(),
        );
        assert_eq!(events.len(), 1, "step 1: must emit meeting-detected");
        assert!(matches!(&events[0], DetectorEvent::MeetingDetected { .. }));
        let state = state; // InCall { cancel_suppressed: false }

        // ── Step 2: Frontend signals cancel, connection drops 8s ───────────
        let suppress = AtomicBool::new(true); // frontend cancel signal set
        let now_8s = conn_seen + Duration::from_secs(8);
        let obs_dropped = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            connection_first_seen_at: None,
            default_title: String::new(),
        };
        // The signal is consumed on this step (compare_exchange true→false)
        let (state, events) = step_detector(state, &obs_dropped, start, now_8s, &suppress, &default_settings());
        assert!(events.is_empty(), "step 2: 8s < debounce → no ended event");
        assert!(matches!(state, DetectorState::InCall { .. }));
        assert!(!suppress.load(Ordering::Acquire), "step 2: suppress signal consumed");

        // ── Step 3: Connection returns → stays InCall, no re-detect ────────
        let now_9s = now_8s + Duration::from_secs(1);
        let obs_returned = DetectorObservation {
            meet_windows: vec![meet_window("Sprint planning - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen + Duration::from_millis(500)),
            default_title: String::new(),
        };
        let (state, events) = step_detector(state, &obs_returned, start, now_9s, &AtomicBool::new(false), &default_settings());
        assert!(events.is_empty(), "step 3: InCall with connection → no re-emit");

        // ── Step 3.5: Connection drops again (0s elapsed < debounce) ──────────
        // This step sets connection_lost_at organically so step 4 can advance time.
        let now_10s = now_9s + Duration::from_secs(1);
        let (state, events) = step_detector(
            state,
            &obs_dropped,
            start,
            now_10s,
            &AtomicBool::new(false),
            &default_settings(),
        );
        assert!(events.is_empty(), "step 3.5: 0s elapsed < debounce → no ended event yet");

        // ── Step 4: Still dropped 12s later → meeting-ended, Idle ─────────────
        let now_22s = now_10s + Duration::from_secs(12);
        let (state, events) = step_detector(
            state,
            &obs_dropped,
            start,
            now_22s,
            &AtomicBool::new(false),
            &default_settings(),
        );
        assert_eq!(events, vec![DetectorEvent::MeetingEnded], "step 4: 12s > debounce → meeting-ended");
        assert!(matches!(state, DetectorState::Idle), "step 4: must return to Idle");

        // ── Step 5: New connection after Idle reset → must re-emit ─────────
        let now_rejoin = now_22s + Duration::from_secs(5);
        let conn_seen_new = now_rejoin;
        let obs_new_call = DetectorObservation {
            meet_windows: vec![meet_window("Sprint planning - Google Meet")],
            has_meet_connection: true,
            connection_first_seen_at: Some(conn_seen_new),
            default_title: "Sprint planning - Google Meet".to_string(),
        };
        let (_, events) = step_detector(
            state,
            &obs_new_call,
            start,
            now_rejoin,
            &AtomicBool::new(false),
            &default_settings(),
        );
        assert_eq!(events.len(), 1, "step 5: cancel flag reset on Idle → new call must re-emit");
        assert!(matches!(&events[0], DetectorEvent::MeetingDetected { default_title, .. } if default_title == "Sprint planning - Google Meet"));
    }
}
