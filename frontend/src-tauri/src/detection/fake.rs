//! Dev/test-only fake `MeetingDetectorPort`.
//!
//! Compiled only under `--features dev-detector`. It backs the
//! `__dev_simulate_meeting` Tauri command so the meeting auto-detect flow can be
//! exercised end-to-end (real state machine, real frontend, real recording)
//! without joining a Google Meet call. The observation is shared between the
//! spawned `spawn_detector` task (which polls `current_state`) and the command
//! handler via an `Arc<Mutex<DetectorObservation>>`.

use crate::ports::meeting_detector::{BrowserWindow, DetectorObservation, MeetingDetectorPort};
use std::sync::{Arc, Mutex};

pub type SharedObservation = Arc<Mutex<DetectorObservation>>;

pub struct FakeMeetingDetector {
    state: SharedObservation,
}

impl FakeMeetingDetector {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(DetectorObservation::default())),
        }
    }

    pub fn handle(&self) -> SharedObservation {
        self.state.clone()
    }
}

impl Default for FakeMeetingDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl MeetingDetectorPort for FakeMeetingDetector {
    fn current_state(&mut self) -> DetectorObservation {
        // Clone under lock so every poll returns a self-consistent snapshot —
        // a concurrent `apply` cannot produce a half-written observation.
        // Recover the guard on poison (a panicking `apply` is not expected in
        // practice) rather than dropping the last known value.
        match self.state.lock() {
            Ok(g) => g.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

/// Managed-state handle wrapped by the `__dev_simulate_meeting` command.
/// Cloned from the same `Arc` that lives inside the spawned detector.
pub struct FakeDetectorHandle(pub SharedObservation);

impl FakeDetectorHandle {
    /// Validate `state` and apply it, rejecting anything other than
    /// `"joined"` / `"left"` BEFORE the shared observation is touched.
    pub fn apply(&self, state: &str, title: Option<&str>) -> Result<(), String> {
        let mut g = self.0.lock().map_err(|e| format!("detector state lock poisoned: {e}"))?;
        match state {
            "joined" => {
                let resolved = title.unwrap_or("Simulated meeting").to_string();
                *g = DetectorObservation {
                    browser_windows: vec![BrowserWindow {
                        hwnd_id: 1,
                        pid: 1,
                        title: resolved.clone(),
                    }],
                    candidate_titles: vec![],
                    has_meet_connection: true,
                    has_browser_capture_session: true,
                    // Strictly after `detector_start` (captured at spawn time) so the
                    // conservative app-start (D15) guard does not suppress detection.
                    connection_first_seen_at: Some(std::time::Instant::now()),
                    default_title: resolved,
                    is_turn_exit: false,
                    // Default false = conservative 15 s debounce. The fake adapter drives
                    // no real bc signal, so without an explicit override every simulated
                    // call behaves as if transient-prone (today's behaviour). A test that
                    // needs the short-debounce path sets this directly on the shared obs.
                    stable_capture: false,
                };
                Ok(())
            }
            "left" => {
                // Full idle snapshot — matches DetectorObservation::default() and the
                // real adapter's idle output. A partial idle would drive a state-machine
                // path the production adapter never produces.
                *g = DetectorObservation::default();
                Ok(())
            }
            other => Err(format!(
                "unknown state {other:?}; expected \"joined\" or \"left\""
            )),
        }
    }
}

#[cfg(all(test, feature = "dev-detector"))]
mod tests {
    use super::*;
    use crate::use_cases::meeting_detection::{
        spawn_detector, DetectorEventEmitter, DetectorSettings,
    };
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;

    /// Minimal recorder emitter. Held via `Arc` so the test can inspect after the
    /// detector task has run (mirrors the `Arc<MockEmitter>` pattern).
    struct RecEmitter {
        detected: Mutex<Vec<String>>,
        ended: Mutex<u32>,
    }

    impl DetectorEventEmitter for Arc<RecEmitter> {
        fn emit_detected(&self, default_title: String, _candidate_titles: Vec<String>) {
            self.detected.lock().unwrap().push(default_title);
        }
        fn emit_ended(&self) {
            *self.ended.lock().unwrap() += 1;
        }
    }

    fn fast_settings() -> DetectorSettings {
        DetectorSettings {
            debounce_duration: Duration::from_millis(50),
            stable_udp_debounce_duration: Duration::from_millis(50),
            turn_debounce_duration: Duration::from_millis(50),
        }
    }

    // 2.1 — join → leave drives the REAL state machine + emitter.
    #[tokio::test]
    async fn test_2_1_join_leave_drives_real_state_machine() {
        let fake = FakeMeetingDetector::new();
        let handle = FakeDetectorHandle(fake.handle());
        let emitter = Arc::new(RecEmitter {
            detected: Mutex::new(vec![]),
            ended: Mutex::new(0),
        });
        let suppress = Arc::new(AtomicBool::new(false));
        let det = spawn_detector(
            fake,
            Arc::clone(&emitter),
            Duration::from_millis(5),
            fast_settings(),
            suppress,
        );

        // Let the polling task start (so detector_start is set), then join.
        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.apply("joined", Some("Smoke")).unwrap();
        tokio::time::sleep(Duration::from_millis(60)).await;
        handle.apply("left", None).unwrap();
        // Exceed the 50 ms UDP debounce so meeting-ended fires.
        tokio::time::sleep(Duration::from_millis(200)).await;
        det.abort();

        let detected = emitter.detected.lock().unwrap();
        assert!(!detected.is_empty(), "must emit meeting-detected");
        assert_eq!(detected[0], "Smoke", "detected title must match");
        assert!(
            *emitter.ended.lock().unwrap() >= 1,
            "must emit meeting-ended after the debounce"
        );
    }

    // 2.2 — unknown state is rejected without mutating the observation.
    #[test]
    fn test_2_2_unknown_state_rejected_no_mutation() {
        let handle = FakeDetectorHandle(Arc::new(Mutex::new(DetectorObservation::default())));
        let before = handle.0.lock().unwrap().clone();

        let err = handle.apply("paused", None);
        assert!(err.is_err(), "unknown state must be rejected");

        let after = handle.0.lock().unwrap().clone();
        assert_eq!(before, after, "observation must not be mutated on rejection");
    }

    // 2.3 / 3.3 — rapid toggling racing the poll loop: no panic, no deadlock,
    // every snapshot self-consistent (Mutex + clone-under-lock).
    #[tokio::test]
    async fn test_2_3_rapid_toggle_no_panic_or_deadlock() {
        let fake = FakeMeetingDetector::new();
        let handle = Arc::new(FakeDetectorHandle(fake.handle()));
        let emitter = Arc::new(RecEmitter {
            detected: Mutex::new(vec![]),
            ended: Mutex::new(0),
        });
        let suppress = Arc::new(AtomicBool::new(false));
        let det = spawn_detector(
            fake,
            Arc::clone(&emitter),
            Duration::from_millis(1),
            DetectorSettings {
                debounce_duration: Duration::from_millis(5),
                stable_udp_debounce_duration: Duration::from_millis(5),
                turn_debounce_duration: Duration::from_millis(5),
            },
            suppress,
        );

        let h = Arc::clone(&handle);
        let toggler = tokio::spawn(async move {
            for i in 0..1000 {
                let _ = if i % 2 == 0 {
                    h.apply("joined", Some("x"))
                } else {
                    h.apply("left", None)
                };
            }
        });

        tokio::time::sleep(Duration::from_millis(150)).await;
        toggler.await.unwrap();
        det.abort();

        // Reaching here means no deadlock/panic. Final snapshot must be a valid,
        // self-consistent observation (clone under a live lock).
        let snap = handle.0.lock().unwrap().clone();
        let _ = format!("{snap:?}");
    }

    // C2 — stable_capture=false holds meeting-ended through LONG: a transient-prone
    // exit observation must NOT fire ended at SHORT elapsed. Pins that the LONG
    // branch is actually reachable through the spawn loop, not only at the pure
    // step_detector altitude. (The short-path C1 test lives in detection/windows.rs
    // so it runs under the default `cargo test --lib`; this C2 test uses the fake
    // adapter and stays dev-detector-gated.)
    #[tokio::test]
    async fn stable_capture_false_holds_ended_through_long_debounce() {
        let fake = FakeMeetingDetector::new();
        let handle = FakeDetectorHandle(fake.handle());
        let emitter = Arc::new(RecEmitter {
            detected: Mutex::new(vec![]),
            ended: Mutex::new(0),
        });
        let suppress = Arc::new(AtomicBool::new(false));
        let settings = DetectorSettings {
            debounce_duration: Duration::from_millis(400),           // LONG
            stable_udp_debounce_duration: Duration::from_millis(60), // SHORT
            turn_debounce_duration: Duration::from_millis(400),
        };
        let det = spawn_detector(
            fake,
            Arc::clone(&emitter),
            Duration::from_millis(5),
            settings,
            suppress,
        );

        tokio::time::sleep(Duration::from_millis(20)).await;
        handle.apply("joined", Some("Long")).unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;

        // Exit observation: transient-prone → stable_capture=false → LONG.
        {
            let mut g = handle.0.lock().unwrap();
            g.has_browser_capture_session = false;
            g.is_turn_exit = false;
            g.stable_capture = false;
        }

        // At SHORT+slack (160ms < LONG=400ms), ended must NOT have fired.
        tokio::time::sleep(Duration::from_millis(160)).await;
        let ended_before_long = *emitter.ended.lock().unwrap();
        det.abort();

        assert_eq!(
            ended_before_long, 0,
            "stable_capture=false must hold meeting-ended until LONG (400ms); ended={ended_before_long}"
        );
    }
}
