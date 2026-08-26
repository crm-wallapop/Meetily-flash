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
    /// lag + 2 s poll granularity + margin.
    ///
    /// 2026-08-26: the separate TCP TURN debounce was removed together with the
    /// TURN socket scan — Meet media is UDP (invisible to the TCP table), so the
    /// scan never latched on real calls and its CIDR ranges generated false
    /// positives. The stable-capture latch provides the same fast exit.
    pub stable_udp_debounce_duration: Duration,
}

impl Default for DetectorSettings {
    fn default() -> Self {
        Self {
            debounce_duration: Duration::from_secs(15),
            stable_udp_debounce_duration: Duration::from_secs(4),
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
        /// time against the stability-specific debounce window (15 s / 4 s).
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
            let has_conn = observation.has_meet_connection;
            // Only fire for connections that appeared after the detector started (D15).
            let not_preexisting = observation
                .connection_first_seen_at
                .map(|t| t > detector_start)
                .unwrap_or(false);

            // Vendor-neutral gate (design §4): entry is `has_conn && not_preexisting`.
            // The title requirement is dropped — detection is driven by the call-signaling
            // + WASAPI conjunction, not window-title matching. The wired
            // MeetingTitleExtractorPort handles title decoration separately.
            if has_conn && not_preexisting {
                let default_title = observation.default_title.clone();
                let candidate_titles = observation.candidate_titles.clone();
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

            // WASAPI active (bc=true) means call is live — clear timer.
            if observation.has_browser_capture_session {
                connection_lost_at = None;
                (
                    DetectorState::InCall { connection_lost_at },
                    vec![],
                )
            } else {
                // The debounce comes from the adapter's locked-first-drop latch
                // (detection/windows.rs): 4 s when the call's first bc drop was
                // preceded by ≥ STABLE_CONFIDENCE_WINDOW of continuous capture
                // (stable_capture=true), 15 s otherwise (short/flaky first-drop run, or no
                // drop yet). The latch is immutable for the rest of the call, so recomputing
                // the debounce every poll is safe — the value cannot flip mid-debounce.
                let debounce = if observation.stable_capture {
                    settings.stable_udp_debounce_duration
                } else {
                    settings.debounce_duration
                };
                let lost_at = connection_lost_at.unwrap_or(now);
                let elapsed = now.duration_since(lost_at);
                log::debug!(
                    "InCall: no connection — debounce {:.1}s / {:.1}s (stable_capture={})",
                    elapsed.as_secs_f32(),
                    debounce.as_secs_f32(),
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
    use crate::ports::meeting_detector::{BrowserWindow, DetectorObservation, MeetingDetectorPort};
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

    fn browser_window(title: &str) -> BrowserWindow {
        BrowserWindow {
            hwnd_id: 1,
            pid: 100,
            title: title.to_string(),
        }
    }

    fn idle_obs() -> DetectorObservation {
        DetectorObservation {
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            stable_capture: false,
        }
    }

    /// An observation that should trigger detection: connection present + fresh.
    fn detected_obs(title: &str, detector_start: Instant) -> DetectorObservation {
        let conn_seen = detector_start + Duration::from_millis(500); // appeared after start
        DetectorObservation {
            browser_windows: vec![browser_window(title)],
            candidate_titles: vec![],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: title.to_string(),
            stable_capture: false,
        }
    }

    fn default_settings() -> DetectorSettings {
        DetectorSettings {
            debounce_duration: Duration::from_secs(15),
            stable_udp_debounce_duration: Duration::from_secs(4),
        }
    }

    fn no_suppress() -> AtomicBool {
        AtomicBool::new(false)
    }

    // ── 2.1 ───────────────────────────────────────────────────────────────
    // Idle → InCall: connection + fresh → emit meeting-detected.
    #[test]
    fn test_2_1_idle_transitions_to_in_call_on_valid_observation() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(500);

        let obs = DetectorObservation {
            browser_windows: vec![browser_window("Meet - Weekly sync")],
            candidate_titles: vec![],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: "Meet - Weekly sync".to_string(),
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
            browser_windows: vec![browser_window("Meet - All-hands")],
            candidate_titles: vec![],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(start),
            default_title: String::new(),
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
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
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
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
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
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
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
            browser_windows: vec![browser_window("Meet - Sync")],
            candidate_titles: vec![],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(start + Duration::from_millis(500)),
            default_title: String::new(),
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
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            stable_capture: false,
        };
        let (state, events) = step_detector(state, &obs_gone, start, now, &AtomicBool::new(false), &default_settings());
        assert!(matches!(state, DetectorState::Idle));
        assert_eq!(events, vec![DetectorEvent::MeetingEnded]);

        // Step 4: new connection → must re-emit.
        let conn_seen = now + Duration::from_millis(500);
        let obs_new = DetectorObservation {
            browser_windows: vec![browser_window("Meet - New call")],
            candidate_titles: vec![],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: "Meet - New call".to_string(),
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
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
            stable_capture: false,
        };
        let obs_true = DetectorObservation {
            browser_windows: vec![browser_window("Meet - Sprint")],
            candidate_titles: vec![],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
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
    // Browser windows present WITHOUT a Meet connection (tab open, user not
    // joined) → Idle. The title-is-present signal is no longer load-bearing
    // (D3); the signaling+bc conjunction is the sole discriminator.
    #[test]
    fn test_2_7_browser_windows_without_connection_stays_idle() {
        let start = Instant::now();
        let obs = DetectorObservation {
            browser_windows: vec![browser_window("Meet - Sync")],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
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
    // §4 gate change: connection present + fresh, even with NO browser windows,
    // now FIRES detection. The title gate is dropped — entry is driven by the
    // call-signaling + WASAPI conjunction alone. This is the known FP-risk trade-off
    // (a non-Meet browser app with mic access could fire); it is documented in the
    // design and accepted as the cost of vendor-neutral detection.
    #[test]
    fn test_2_8_connection_without_title_fires_detection() {
        let start = Instant::now();
        let conn_seen = start + Duration::from_millis(500);
        let obs = DetectorObservation {
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: true,
            has_browser_capture_session: true,
            connection_first_seen_at: Some(conn_seen),
            default_title: String::new(),
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

        assert!(matches!(state, DetectorState::InCall { .. }), "connection + fresh fires without title gate");
        assert_eq!(events.len(), 1, "must emit meeting-detected");
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
            browser_windows: vec![browser_window("Meet - Standup")],
            candidate_titles: vec![],
            has_meet_connection: false,         // TCP dropped (90s+ UDP call)
            has_browser_capture_session: true,  // WASAPI capture still Active
            connection_first_seen_at: None,
            default_title: String::new(),
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

    // ── meeting-udp-media-signal — step_detector adaptive debounce ──────────
    //
    // The debounce is selected from `stable_capture`: 4 s when true (stable-mic,
    // the common case), 15 s when false (transient-prone, or the adapter has not
    // populated the flag). These tests pin the selection by probing elapsed times
    // that discriminate 4 s from 15 s.

    fn obs_udp_exit(stable_capture: bool) -> DetectorObservation {
        // bc=false so the InCall exit branch engages; stable_capture is the
        // variable under test.
        DetectorObservation {
            browser_windows: vec![],
            candidate_titles: vec![],
            has_meet_connection: false,
            has_browser_capture_session: false,
            connection_first_seen_at: None,
            default_title: String::new(),
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
    // stable_capture (4 s stable / 15 s transient-prone). 2026-08-26: the TURN
    // fast path was removed with the TCP socket scan; the stable-capture latch
    // provides the same 4 s latency for typical calls.
    #[test]
    fn debounce_selection_invariant_matrix() {
        // Stable call: fires at 5 s (4 s debounce).
        let (s, ev) = step_incall_lost(5, &obs_udp_exit(true));
        assert!(matches!(s, DetectorState::Idle),
            "stable at 5 s must fire (4 s debounce)");
        assert_eq!(ev, vec![DetectorEvent::MeetingEnded]);

        // Transient-prone call: does NOT fire at 5 s or 12 s (15 s debounce).
        let (s, _) = step_incall_lost(5, &obs_udp_exit(false));
        assert!(matches!(s, DetectorState::InCall { .. }),
            "transient-prone at 5 s must NOT fire (15 s debounce)");
        let (s, _) = step_incall_lost(12, &obs_udp_exit(false));
        assert!(matches!(s, DetectorState::InCall { .. }),
            "transient-prone at 12 s must NOT fire (15 s debounce)");

        // Discriminator boundary: stable fires at 12 s (12 >= 4).
        let (s_stable, _) = step_incall_lost(12, &obs_udp_exit(true));
        assert!(matches!(s_stable, DetectorState::Idle), "stable at 12 s must fire");
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
        let settings = default_settings(); // SHORT=4 s, LONG=15 s
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
