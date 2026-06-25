use crate::ports::meeting_detector::{DetectorObservation, MeetingDetectorPort};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

// ── Public types ─────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct DetectorSettings {
    /// Debounce for UDP calls on a transient-prone setup (`stable_capture = false`):
    /// how long `has_browser_capture_session` must be absent before a meeting-ended
    /// event fires. 15 s absorbs the empirical ~10 s WASAPI Inactive transients
    /// (Chrome mic re-acquisition) with 5 s margin. Also the conservative default
    /// when the adapter has not populated `stable_capture`.
    pub debounce_duration: Duration,
    /// Debounce for UDP calls classified stable by the adapter's locked-first-drop
    /// latch (`stable_capture = true`): the call's first `has_browser_capture_session`
    /// drop was preceded by ≥ `STABLE_CONFIDENCE_WINDOW` of continuous capture, so it
    /// is a high-confidence leave signal. 4 s absorbs the ~1–2 s getUserMedia release
    /// lag + 2 s poll granularity + margin, matching the empirically validated TURN
    /// debounce.
    pub stable_udp_debounce_duration: Duration,
    /// Debounce for TCP TURN calls (`is_turn_exit = true`): shorter because TURN is a
    /// reliable signal (drops within ~1 s of leaving) and transient TURN blips recover
    /// within 1–2 polls. 4 s is the original debounce value and is sufficient.
    pub turn_debounce_duration: Duration,
}

impl Default for DetectorSettings {
    fn default() -> Self {
        Self {
            debounce_duration: Duration::from_secs(15),
            stable_udp_debounce_duration: Duration::from_secs(4),
            turn_debounce_duration: Duration::from_secs(4),
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
        /// When the connection was first observed as absent. Used to measure elapsed
        /// time against the path-specific debounce window (15 s UDP / 4 s TURN).
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

            // For UDP calls: WASAPI active (bc=true) means call is live — clear timer.
            // For TURN calls: once TURN drops (is_turn_exit=true), start the fast TURN
            // debounce immediately even if bc is still true (WASAPI lags ~2s behind TURN).
            // A returning TURN signal flips is_turn_exit back to false, clearing the timer.
            if !observation.is_turn_exit && observation.has_browser_capture_session {
                connection_lost_at = None;
                (
                    DetectorState::InCall { connection_lost_at },
                    vec![],
                )
            } else {
                // TURN path (is_turn_exit) gates first — stable_capture is ignored there.
                // UDP path selects the debounce from the adapter's locked-first-drop latch
                // (detection/windows.rs, design D1–D3): 4 s when the call's first bc drop was
                // preceded by ≥ STABLE_CONFIDENCE_WINDOW of continuous capture
                // (stable_capture=true), 15 s otherwise (short/flaky first-drop run, or no
                // drop yet). The latch is immutable for the rest of the call, so recomputing
                // the debounce every poll is safe — the value cannot flip mid-debounce.
                let debounce = if observation.is_turn_exit {
                    settings.turn_debounce_duration
                } else if observation.stable_capture {
                    settings.stable_udp_debounce_duration
                } else {
                    settings.debounce_duration
                };
                let lost_at = connection_lost_at.unwrap_or(now);
                let elapsed = now.duration_since(lost_at);
                log::debug!(
                    "InCall: no connection — debounce {:.1}s / {:.1}s (turn_exit={} stable_capture={})",
                    elapsed.as_secs_f32(),
                    debounce.as_secs_f32(),
                    observation.is_turn_exit,
                    observation.stable_capture,
                );
                if elapsed >= debounce {
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
                    DetectorEvent::MeetingEnded => {
                        // notify_exit() before emit_ended(): adapter state is consistent
                        // if emit_ended() errors, and turn_established cannot be left true
                        // if this task is aborted between the two calls.
                        port.notify_exit();
                        emitter.emit_ended();
                    }
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
        /// Shared counter incremented each time `notify_exit()` is called.
        /// Tests that need to verify the use case calls the callback hold an `Arc` clone.
        pub notify_exit_calls: Arc<Mutex<u32>>,
    }

    impl MockMeetingDetector {
        pub fn new(sequence: impl IntoIterator<Item = DetectorObservation>) -> Self {
            let q: VecDeque<DetectorObservation> = sequence.into_iter().collect();
            let fallback = q.back().cloned().unwrap_or_else(idle_obs);
            Self {
                observations: Mutex::new(q),
                fallback,
                notify_exit_calls: Arc::new(Mutex::new(0)),
            }
        }
    }

    impl MeetingDetectorPort for MockMeetingDetector {
        fn current_state(&mut self) -> DetectorObservation {
            let mut q = self.observations.lock().unwrap();
            q.pop_front().unwrap_or_else(|| self.fallback.clone())
        }

        fn notify_exit(&mut self) {
            *self.notify_exit_calls.lock().unwrap() += 1;
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
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
        }
    }

    /// An observation that should trigger detection: title match + fresh connection.
    fn detected_obs(title: &str, detector_start: Instant) -> DetectorObservation {
        let conn_seen = detector_start + Duration::from_millis(500); // appeared after start
        DetectorObservation {
            meet_windows: vec![meet_window(title)],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: title.to_string(),
            is_turn_exit: false,
            stable_capture: false,
        }
    }

    fn default_settings() -> DetectorSettings {
        DetectorSettings {
            debounce_duration: Duration::from_secs(15),
            stable_udp_debounce_duration: Duration::from_secs(4),
            turn_debounce_duration: Duration::from_secs(4),
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
            meet_windows: vec![meet_window("Meet - Weekly sync")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: "Meet - Weekly sync".to_string(),
            is_turn_exit: false,
            stable_capture: false,
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
            matches!(&events[0], DetectorEvent::MeetingDetected { default_title, .. } if default_title == "Meet - Weekly sync")
        );
    }

    // ── 2.2 ───────────────────────────────────────────────────────────────
    // App-start state (D15): connection was already present at detector start → no event.
    #[test]
    fn test_2_2_preexisting_connection_does_not_fire() {
        let start = Instant::now();
        // connection_first_seen_at == detector_start_time → pre-existing
        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Meet - All-hands")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(start),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
        // connection lost 5 seconds ago (< 15s UDP debounce)
        let lost_5s_ago = now - Duration::from_secs(5);

        let obs = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
        // connection lost 16 seconds ago (> 15s UDP debounce)
        let lost_11s_ago = now - Duration::from_secs(16);

        let obs = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
            meet_windows: vec![meet_window("Meet - Sync")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(start + Duration::from_millis(500)),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
        };
        let (state, events) = step_detector(state, &obs_back, start, Instant::now(), &AtomicBool::new(false), &default_settings());
        assert!(events.is_empty(), "no re-emit after transient drop+return");
        assert!(matches!(state, DetectorState::InCall { .. }));

        // Step 3: connection drops for > 15s → transition to Idle.
        let now = Instant::now();
        let lost_11s_ago = now - Duration::from_secs(16);
        let state = DetectorState::InCall {
            connection_lost_at: Some(lost_11s_ago),
        };
        let obs_gone = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
        };
        let (state, events) = step_detector(state, &obs_gone, start, now, &AtomicBool::new(false), &default_settings());
        assert!(matches!(state, DetectorState::Idle));
        assert_eq!(events, vec![DetectorEvent::MeetingEnded]);

        // Step 4: new connection → must re-emit.
        let conn_seen = now + Duration::from_millis(500);
        let obs_new = DetectorObservation {
            meet_windows: vec![meet_window("Meet - New call")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: "Meet - New call".to_string(),
            is_turn_exit: false,
            stable_capture: false,
        };
        let (_, events) = step_detector(state, &obs_new, start, conn_seen, &AtomicBool::new(false), &default_settings());
        assert_eq!(events.len(), 1, "new call after Idle reset must re-emit");
        assert!(matches!(&events[0], DetectorEvent::MeetingDetected { default_title, .. } if default_title == "Meet - New call"));
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
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
        };
        let obs_true = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Sprint")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
        assert!(e.is_empty(), "total 3s < 15s UDP debounce → no ended");
    }

    // ── 2.7 ───────────────────────────────────────────────────────────────
    // Title match WITHOUT has_meet_connection (Meet tab open, user not joined) → Idle.
    #[test]
    fn test_2_7_title_without_connection_stays_idle() {
        let start = Instant::now();
        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Sync")],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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

    // ── 2.9 ───────────────────────────────────────────────────────────────
    // Asymmetric exit signal: TCP drops (has_meet_connection=false) during an
    // active UDP call, but WASAPI capture is still Active (has_browser_capture_session=true).
    // → InCall must stay InCall with connection_lost_at cleared.
    //
    // This is the primary regression guard for the fix-meeting-ended-udp-calls change.
    // The exit signal is WASAPI (bc), not the TCP conjunction. A 90s+ TCP drop that
    // occurs during an active UDP call must not trigger meeting-ended. If this test
    // is deleted and the InCall branch reverted to `has_meet_connection`, all other
    // InCall tests would still pass while this bug silently regresses.
    #[test]
    fn test_2_9_tcp_drop_during_active_wasapi_stays_incall() {
        let now = Instant::now();
        // Debounce timer was already started by a prior poll (bc was false).
        // Even 14 s in — one second short of the 15 s threshold — a bc=true
        // observation must clear the timer and keep us InCall.
        let lost_14s_ago = now - Duration::from_secs(14);

        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Standup")],
            has_meet_connection: false,         // TCP dropped (90s+ UDP call)
            has_browser_capture_session: true,  // WASAPI capture still Active
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
        };

        let state = DetectorState::InCall {
            connection_lost_at: Some(lost_14s_ago), // debounce was one second from expiry
        };

        let (new_state, events) = step_detector(
            state,
            &obs,
            now - Duration::from_secs(120),
            now,
            &no_suppress(),
            &default_settings(),
        );

        // bc=true must clear the debounce timer and keep us InCall with no events.
        assert!(
            matches!(new_state, DetectorState::InCall { connection_lost_at: None }),
            "bc=true must clear connection_lost_at even when mc=false (TCP drop during UDP call)"
        );
        assert!(
            events.is_empty(),
            "WASAPI active during TCP drop must not emit meeting-ended"
        );
    }

    // ── 2.10 ──────────────────────────────────────────────────────────────
    // TURN exit uses the fast 4 s debounce, not the 15 s UDP debounce.
    // is_turn_exit=true means TURN was established for this call and has just
    // dropped. The event must fire after 4 s, not 15 s.
    #[test]
    fn test_2_10_turn_exit_fires_fast_4s_debounce() {
        let now = Instant::now();
        let obs = DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: true,
            stable_capture: false,
        };

        // 3 s elapsed — still under the 4 s TURN debounce.
        let state = DetectorState::InCall { connection_lost_at: Some(now - Duration::from_secs(3)) };
        let (new_state, events) = step_detector(
            state, &obs, now - Duration::from_secs(60), now,
            &no_suppress(), &default_settings(),
        );
        assert!(matches!(new_state, DetectorState::InCall { .. }), "3s < 4s TURN debounce → stay InCall");
        assert!(events.is_empty(), "no event before 4s TURN debounce");

        // 5 s elapsed — past the 4 s TURN debounce, must fire.
        let state = DetectorState::InCall { connection_lost_at: Some(now - Duration::from_secs(5)) };
        let (new_state, events) = step_detector(
            state, &obs, now - Duration::from_secs(60), now,
            &no_suppress(), &default_settings(),
        );
        assert!(matches!(new_state, DetectorState::Idle), "5s > 4s TURN debounce → Idle");
        assert_eq!(events, vec![DetectorEvent::MeetingEnded], "TURN exit fires after 4s debounce");
    }

    // ── 2.10b ─────────────────────────────────────────────────────────────
    // TURN drops but WASAPI is still active (the typical Chrome exit path: TURN
    // drops within ~1 s of leave, WASAPI holds for another ~1–2 s).
    // is_turn_exit=true, bc=true → the TURN debounce must START (not be cleared).
    // This covers the quadrant that is most likely to regress if someone reads
    // "bc=true → clear timer" from the UDP path and applies it uniformly.
    #[test]
    fn test_2_10b_turn_exit_bc_still_active_starts_debounce_not_clears_it() {
        let now = Instant::now();

        // TURN just dropped; WASAPI still streaming.
        let obs = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Standup")],
            has_meet_connection: false,
            has_browser_capture_session: true,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: true,
            stable_capture: false,
        };

        // Step 1: timer not yet started (connection_lost_at=None) — first poll after TURN drop.
        let state = DetectorState::InCall { connection_lost_at: None };
        let (new_state, events) = step_detector(
            state, &obs, now - Duration::from_secs(60), now,
            &no_suppress(), &default_settings(),
        );
        // Timer must have been STARTED (Some), not cleared.
        assert!(
            matches!(new_state, DetectorState::InCall { connection_lost_at: Some(_) }),
            "is_turn_exit=true must start the debounce timer even when bc=true (WASAPI lags)"
        );
        assert!(events.is_empty(), "no event on first poll after TURN drop");

        // Step 2: 5 s elapsed — past the 4 s TURN debounce, must fire meeting-ended.
        let state = DetectorState::InCall { connection_lost_at: Some(now - Duration::from_secs(5)) };
        let (new_state, events) = step_detector(
            state, &obs, now - Duration::from_secs(60), now,
            &no_suppress(), &default_settings(),
        );
        assert!(matches!(new_state, DetectorState::Idle), "5s > 4s TURN debounce → Idle");
        assert_eq!(events, vec![DetectorEvent::MeetingEnded], "meeting-ended fires after 4s TURN debounce");
    }

    // ── 2.11 ──────────────────────────────────────────────────────────────
    // TURN transient blip: TURN drops briefly (is_turn_exit=true) then returns.
    // When TURN returns, is_turn_exit flips to false and the timer must clear.
    #[test]
    fn test_2_11_turn_transient_blip_clears_debounce_timer() {
        let now = Instant::now();

        // TURN debounce was started 3 s ago but TURN has now returned.
        let state = DetectorState::InCall { connection_lost_at: Some(now - Duration::from_secs(3)) };
        let obs_turn_back = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Standup")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(now - Duration::from_secs(60)),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
        };

        let (new_state, events) = step_detector(
            state, &obs_turn_back, now - Duration::from_secs(60), now,
            &no_suppress(), &default_settings(),
        );

        assert!(
            matches!(new_state, DetectorState::InCall { connection_lost_at: None }),
            "TURN return must clear connection_lost_at"
        );
        assert!(events.is_empty(), "TURN blip return must not emit meeting-ended");
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
            meet_windows: vec![meet_window("Meet - Standup")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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

    // ── 5.1a ──────────────────────────────────────────────────────────────
    // spawn_detector emits meeting-ended after has_meet_connection stays false
    // for > debounce duration, and calls port.notify_exit() exactly once.
    #[tokio::test]
    async fn test_5_1a_udp_exit_emits_meeting_ended_and_calls_notify_exit() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(5);
        let in_call = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Standup")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: "Meet - Standup".to_string(),
            is_turn_exit: false,
            stable_capture: false,
        };
        // 3 polls "in call", then idle. MockMeetingDetector repeats the last item as
        // fallback, so a single idle_obs() at the end provides idle state indefinitely.
        let port = MockMeetingDetector::new(
            std::iter::repeat(in_call).take(3).chain(std::iter::once(idle_obs())),
        );
        let notify_calls = Arc::clone(&port.notify_exit_calls);

        let emitter = Arc::new(MockEmitter::default());
        let emitter_clone = Arc::clone(&emitter);
        let suppress = Arc::new(AtomicBool::new(false));

        let handle = spawn_detector(
            port,
            emitter_clone,
            Duration::from_millis(5),
            DetectorSettings { debounce_duration: Duration::from_millis(50), stable_udp_debounce_duration: Duration::from_millis(50), turn_debounce_duration: Duration::from_millis(50) },
            suppress,
        );

        tokio::time::sleep(Duration::from_millis(400)).await;
        handle.abort();

        assert!(*emitter.ended_count.lock().unwrap() >= 1,
            "meeting-ended must fire after UDP connection absent > debounce");
        assert_eq!(*notify_calls.lock().unwrap(), 1,
            "spawn_detector must call port.notify_exit() exactly once per meeting-ended");
    }

    // ── 4.5 ───────────────────────────────────────────────────────────────
    // A panicking port must not crash the spawner loop. After the panic is caught
    // the loop must continue polling, eventually emitting when the port recovers.
    #[tokio::test]
    async fn test_4_5_port_panic_does_not_crash_detector() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(100);
        let success_obs = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Resilience")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
            DetectorSettings { debounce_duration: Duration::from_secs(10), stable_udp_debounce_duration: Duration::from_secs(10), turn_debounce_duration: Duration::from_secs(10) },
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
            meet_windows: vec![meet_window("Meet - All-hands")],
            has_meet_connection: false, // Spotify: no Google-IP connection
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
            meet_windows: vec![meet_window("Meet - Team call")],
            has_meet_connection: false, // Discord: connection is not to Google IPs
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
            meet_windows: vec![meet_window("Meet - Q4 review")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            // connection_first_seen_at == detector_start → pre-existing (D15)
            connection_first_seen_at: Some(detector_start),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
    // 3. connection drops 8s (< 15s debounce) → no ended, no re-detect
    // 4. connection returns → stays InCall, no re-detect (cancel-suppressed)
    // 5. connection drops 12s (< 15s debounce) then 16s total (> 15s) → meeting-ended, Idle
    // 6. new connection → meeting-detected fires again (flag was reset)
    #[test]
    fn test_4_9_cancel_suppression_full_scripted_sequence() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(500);

        // ── Step 1: Idle → InCall, meeting-detected fires ──────────────────
        let obs_detected = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Sprint planning")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
        };
        // The signal is consumed on this step (compare_exchange true→false)
        let (state, events) = step_detector(state, &obs_dropped, start, now_8s, &suppress, &default_settings());
        assert!(events.is_empty(), "step 2: 8s < debounce → no ended event");
        assert!(matches!(state, DetectorState::InCall { .. }));
        assert!(!suppress.load(Ordering::Acquire), "step 2: suppress signal consumed");

        // ── Step 3: Connection returns → stays InCall, no re-detect ────────
        let now_9s = now_8s + Duration::from_secs(1);
        let obs_returned = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Sprint planning")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen + Duration::from_millis(500)),
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture: false,
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

        // ── Step 4: Still dropped 16s later → meeting-ended, Idle ─────────────
        let now_22s = now_10s + Duration::from_secs(16);
        let (state, events) = step_detector(
            state,
            &obs_dropped,
            start,
            now_22s,
            &AtomicBool::new(false),
            &default_settings(),
        );
        assert_eq!(events, vec![DetectorEvent::MeetingEnded], "step 4: 16s > 15s debounce → meeting-ended");
        assert!(matches!(state, DetectorState::Idle), "step 4: must return to Idle");

        // ── Step 5: New connection after Idle reset → must re-emit ─────────
        let now_rejoin = now_22s + Duration::from_secs(5);
        let conn_seen_new = now_rejoin;
        let obs_new_call = DetectorObservation {
            meet_windows: vec![meet_window("Meet - Sprint planning")],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen_new),
            default_title: "Meet - Sprint planning".to_string(),
            is_turn_exit: false,
            stable_capture: false,
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
        assert!(matches!(&events[0], DetectorEvent::MeetingDetected { default_title, .. } if default_title == "Meet - Sprint planning"));
    }

    // ── meeting-udp-media-signal — step_detector adaptive debounce ──────────
    //
    // The UDP debounce is selected from `stable_capture`: 4 s when true
    // (stable-mic, the common case), 15 s when false (transient-prone, or the
    // adapter has not populated the flag). The TURN path (is_turn_exit=true) is
    // invariant under `stable_capture`. These tests pin the selection by probing
    // elapsed times that discriminate 4 s from 15 s.

    fn obs_udp_exit(stable_capture: bool) -> DetectorObservation {
        // bc=false so the InCall exit branch engages; is_turn_exit=false so the
        // UDP (not TURN) path runs; stable_capture is the variable under test.
        DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: false,
            stable_capture,
        }
    }

    fn obs_turn_exit(stable_capture: bool) -> DetectorObservation {
        // TURN drop: is_turn_exit=true. bc is irrelevant (TURN gates first).
        DetectorObservation {
            meet_windows: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            is_turn_exit: true,
            stable_capture,
        }
    }

    fn step_incall_lost(lost_secs_before_now: u64, obs: &DetectorObservation) -> (DetectorState, Vec<DetectorEvent>) {
        let now = Instant::now();
        let lost_at = now - Duration::from_secs(lost_secs_before_now);
        let state = DetectorState::InCall { connection_lost_at: Some(lost_at) };
        step_detector(state, obs, now - Duration::from_secs(600), now, &AtomicBool::new(false), &default_settings())
    }

    // Task 1.4 — A stable-mic UDP call (stable_capture=true) exits on the SHORT
    // (4 s) debounce. At 5 s elapsed, meeting-ended MUST fire (5 ≥ 4). With the
    // pre-change code, only the 15 s debounce exists, so 5 s would NOT fire.
    #[test]
    fn stable_call_uses_short_udp_debounce() {
        let (new_state, events) = step_incall_lost(5, &obs_udp_exit(true));
        assert!(matches!(new_state, DetectorState::Idle),
            "stable_capture=true at 5 s elapsed: must fire meeting-ended (SHORT=4 s)");
        assert_eq!(events, vec![DetectorEvent::MeetingEnded]);
    }

    // Task 1.5 — A transient-prone UDP call (stable_capture=false) keeps the
    // LONG (15 s) debounce. At 10 s elapsed, meeting-ended must NOT fire
    // (10 < 15). This pins the preserved behaviour.
    #[test]
    fn transient_prone_call_uses_long_udp_debounce() {
        let (new_state, events) = step_incall_lost(10, &obs_udp_exit(false));
        assert!(matches!(new_state, DetectorState::InCall { .. }),
            "stable_capture=false at 10 s elapsed: must NOT fire (LONG=15 s)");
        assert!(events.is_empty());
    }

    // Task 1.6 — Invariant matrix: the debounce is a pure function of
    // (is_turn_exit, stable_capture). TURN path (4 s) is invariant under
    // stable_capture; UDP path selects 4 s only when stable_capture=true.
    #[test]
    fn debounce_selection_invariant_matrix() {
        for &stable in &[false, true] {
            // TURN path: fires at 5 s regardless of stable_capture.
            let (s, ev) = step_incall_lost(5, &obs_turn_exit(stable));
            assert!(matches!(s, DetectorState::Idle),
                "TURN path: stable_capture={stable} at 5 s must fire (4 s debounce, invariant)");
            assert_eq!(ev, vec![DetectorEvent::MeetingEnded]);
        }

        // UDP path, stable=true: fires at 5 s (4 s debounce).
        let (s, _) = step_incall_lost(5, &obs_udp_exit(true));
        assert!(matches!(s, DetectorState::Idle),
            "UDP stable at 5 s must fire (4 s debounce)");

        // UDP path, stable=false: does NOT fire at 5 s (15 s debounce).
        let (s, _) = step_incall_lost(5, &obs_udp_exit(false));
        assert!(matches!(s, DetectorState::InCall { .. }),
            "UDP transient-prone at 5 s must NOT fire (15 s debounce)");

        // Discriminator at 12 s: stable fires (12 ≥ 4), transient-prone does not (12 < 15).
        let (s_stable, _) = step_incall_lost(12, &obs_udp_exit(true));
        assert!(matches!(s_stable, DetectorState::Idle), "UDP stable at 12 s must fire");
        let (s_transient, _) = step_incall_lost(12, &obs_udp_exit(false));
        assert!(matches!(s_transient, DetectorState::InCall { .. }), "UDP transient at 12 s must NOT fire");
    }

    // Task 3.2 — stable_capture=true held across multiple InCall exit polls keeps
    // the 4 s debounce selection stable. step_detector recomputes the debounce every
    // poll, so this asserts per-poll recompute is safe given the latch's immutability
    // (design D3 / §1.5): the debounce does not flip mid-exit. Drives the pure state
    // machine through a real-clock sequence (Instant::now base + Duration offsets).
    #[test]
    fn step_detector_stable_capture_drives_4s_when_latched() {
        let obs = obs_udp_exit(true); // bc dropped, UDP path, stable_capture=true
        let start = Instant::now();
        let settings = default_settings(); // SHORT=4 s, LONG=15 s, TURN=4 s
        let suppress = AtomicBool::new(false);
        let mut state = DetectorState::InCall { connection_lost_at: Some(start) };

        // Poll at 1 s, 2 s, 3 s: within the 4 s debounce → InCall, no event.
        for elapsed_secs in [1u64, 2, 3] {
            let now = start + Duration::from_secs(elapsed_secs);
            let (next, events) =
                step_detector(state, &obs, start, now, &suppress, &settings);
            assert!(
                matches!(next, DetectorState::InCall { .. }),
                "at {elapsed_secs} s (< 4 s): must stay InCall — debounce not yet elapsed"
            );
            assert!(events.is_empty(), "at {elapsed_secs} s: no meeting-ended yet");
            state = next;
        }

        // Poll at 4 s: debounce elapsed → Idle + MeetingEnded. The 4 s selection held
        // stable across every poll (a mid-exit flip to 15 s would suppress this).
        let now = start + Duration::from_secs(4);
        let (next, events) = step_detector(state, &obs, start, now, &suppress, &settings);
        assert!(
            matches!(next, DetectorState::Idle),
            "at 4 s with stable_capture=true: must fire (4 s debounce held stable)"
        );
        assert_eq!(events, vec![DetectorEvent::MeetingEnded]);
    }
}
