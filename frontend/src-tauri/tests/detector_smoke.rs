//! Manual smoke tests for the real `WindowsMeetingDetector` on Windows.
//!
//! These tests exercise the live Win32 APIs — they require a real desktop session
//! with Chrome/Edge open and (for the 60-second test) an active Google Meet call.
//!
//! # How to run
//!
//! ```text
//! cargo test --test detector_smoke -p meetily-flash -- --ignored --nocapture
//! ```
//!
//! To run a single test:
//!
//! ```text
//! cargo test --test detector_smoke -p meetily-flash -- --ignored --nocapture enumerate_browser_windows_smoke
//! cargo test --test detector_smoke -p meetily-flash -- --ignored --nocapture detector_60s_smoke
//! ```
//!
//! # Setup
//!
//! ## `enumerate_browser_windows_smoke` (window enumeration + network signals)
//!
//!   1. Open Chrome or Microsoft Edge.
//!   2. Navigate to <https://meet.google.com> — leave the tab open on the lobby
//!      page (you do NOT need to join a call).
//!   3. To test the PWA path:
//!      a. In Chrome, click the ⊕ install icon in the address bar and install
//!         "Google Meet".
//!      b. Launch the PWA from Start or from the Chrome Apps page.
//!         The window title should read something like:
//!         `"Google Meet - Meet — Google Meet"` (note the em dash U+2014).
//!   4. Run the test.  Read the output:
//!      - At least one window should appear with a title containing "Google Meet".
//!      - For the PWA: the title must start with `"Google Meet - Meet "`.
//!
//! ## `detector_60s_smoke` (full 60-second state-machine trace)
//!
//!   1. Do the setup above (PWA preferred).
//!   2. Join a real Google Meet call (video/audio session, not just the lobby).
//!   3. Run the test.  The expected trace:
//!      - Polls 1–5  (~0–10 s): state=Idle, `has_conn=false` (signalling phase) or
//!                               `has_conn=true` (TURN already established).
//!      - First `has_conn=true` poll: `state→InCall`, EVENT: MeetingDetected printed.
//!      - Subsequent polls while in call: state=InCall, `has_conn=true`.
//!      - Leave the call at ~30 s.  Observe `has_conn=false` and `InCall*` debounce.
//!      - After 10 s of `has_conn=false`: EVENT: MeetingEnded, `state→Idle`.
//!   4. If the call was already active BEFORE the test started, D15 applies:
//!      the detector MUST stay Idle for that call (app-start guard).
//!      Leave and rejoin; the detector should fire for the new join.

#![cfg(target_os = "windows")]

use std::collections::VecDeque;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use app_lib::detection::windows::{
    enumerate_browser_windows, has_turn_connection, FocusHistory,
    WindowsMeetingDetector,
};
use app_lib::detection::signaling::meet::MeetSignalingAdapter;
use app_lib::detection::titles::meet::MeetTitleExtractor;
use app_lib::ports::call_signaling::CallSignalingPort;
use app_lib::ports::meeting_detector::MeetingDetectorPort;
use app_lib::use_cases::meeting_detection::{
    step_detector, DetectorEvent, DetectorSettings, DetectorState,
};

// ── Helpers ────────────────────────────────────────────────────────────────

fn empty_focus_history() -> FocusHistory {
    Arc::new(Mutex::new(VecDeque::new()))
}

/// Initialises env_logger so internal `log::debug!` calls in the adapter and
/// state machine are visible when `RUST_LOG` is set.  Safe to call from multiple
/// tests — only the first call takes effect.
///
/// Usage: set `RUST_LOG=app_lib::detection=debug,app_lib::use_cases=debug`
/// (or just `RUST_LOG=debug` for everything) before running the test.
fn init_logger() {
    let _ = env_logger::builder().is_test(true).try_init();
}

fn state_label(s: &DetectorState) -> &'static str {
    match s {
        DetectorState::Idle => "Idle    ",
        DetectorState::InCall { connection_lost_at: None } => "InCall  ",
        DetectorState::InCall { connection_lost_at: Some(_) } => "InCall* ", // debouncing
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

/// Calls `enumerate_browser_windows()` and the two network-signal functions, then
/// prints what is visible right now.
///
/// Pass criteria (human review):
///   - No panic, no Rust error in the output.
///   - If a Chrome/Edge Meet tab is open, at least one window appears.
///   - If the Google Meet PWA is running, a window with a title starting
///     `"Google Meet - Meet "` (em dash U+2014 expected after "Meet") must appear.
///   - `has_turn_connection` should be `true` only while inside a live call.
#[test]
#[ignore]
fn enumerate_browser_windows_smoke() {
    init_logger();
    let windows = enumerate_browser_windows();
    println!("\n── enumerate_browser_windows_smoke ─────────────────────────────────");
    println!("  Windows found: {}", windows.len());
    for (i, w) in windows.iter().enumerate() {
        let pwa = w.title.starts_with("Google Meet - Meet ");
        println!(
            "  [{i}] pid={:<6} hwnd={:#010x}  {}  title={:?}",
            w.pid,
            w.hwnd_id,
            if pwa { "PWA " } else { "tab " },
            w.title,
        );
    }

    let signaling = MeetSignalingAdapter::new().is_call_signaling_active();
    let turn = has_turn_connection();
    println!("\n  signaling_active    (broad Google IPs) : {signaling}");
    println!("  has_turn_connection  (TURN relay IPs)   : {turn}");

    if windows.is_empty() {
        println!("\n  NOTE: no Meet windows — open Chrome/Edge at meet.google.com first.");
        println!("        For PWA: install via Chrome's ⊕ icon, then launch from Start.");
    }

    let pwa_count = windows
        .iter()
        .filter(|w| w.title.starts_with("Google Meet - Meet "))
        .count();
    println!("\n  PWA windows: {pwa_count}");
    println!("──────────────────────────────────────────────────────────────────");
}

/// Runs the real `WindowsMeetingDetector` for 60 seconds (30 polls × 2 s) and
/// traces every observation and state transition.
///
/// Pass criteria (human review — see module doc-comment for the expected trace).
/// The test itself only asserts that 30 poll iterations complete without panic.
#[test]
#[ignore]
fn detector_60s_smoke() {
    init_logger();
    const TOTAL_POLLS: u32 = 30;
    const POLL_INTERVAL: Duration = Duration::from_secs(2);

    let mut port = WindowsMeetingDetector::new(
        empty_focus_history(),
        Arc::new(MeetSignalingAdapter::new()),
        Arc::new(MeetTitleExtractor::new()),
    );
    let suppress = Arc::new(AtomicBool::new(false));
    let settings = DetectorSettings::default(); // 10-s debounce
    let detector_start = Instant::now();
    let mut state = DetectorState::Idle;
    let mut polls_completed: u32 = 0;

    println!("\n── detector_60s_smoke ─────────────────────────────────────────────");
    println!("  Polling every 2 s for 60 s.");
    println!("  Join a Meet call to trigger MeetingDetected.");
    println!("  Leave the call after ~30 s to observe the 10-s debounce → MeetingEnded.");
    println!("  InCall* = InCall while debouncing a connection drop.");
    println!("────────────────────────────────────────────────────────────────────");

    for i in 0..TOTAL_POLLS {
        let elapsed = detector_start.elapsed();
        let obs = port.current_state();
        let now = Instant::now();

        let (next_state, events) = step_detector(
            state.clone(),
            &obs,
            detector_start,
            now,
            &suppress,
            &settings,
        );

        let win_titles: Vec<&str> = obs.browser_windows.iter().map(|w| w.title.as_str()).collect();
        println!(
            "  [{:02}] t={:5.1}s  {}  has_conn={}  turn={}  windows={}",
            i + 1,
            elapsed.as_secs_f32(),
            state_label(&next_state),
            obs.has_meet_connection,
            has_turn_connection(),
            if win_titles.is_empty() {
                "[]".to_string()
            } else {
                format!("{:?}", win_titles)
            },
        );

        if !obs.default_title.is_empty() {
            println!("         default_title={:?}", obs.default_title);
        }

        for event in &events {
            match event {
                DetectorEvent::MeetingDetected {
                    default_title,
                    candidate_titles,
                } => {
                    println!(
                        "  *** EVENT: MeetingDetected  title={:?}  candidates={:?}",
                        default_title, candidate_titles
                    );
                }
                DetectorEvent::MeetingEnded => {
                    println!("  *** EVENT: MeetingEnded");
                }
            }
        }

        state = next_state;
        polls_completed += 1;
        std::thread::sleep(POLL_INTERVAL);
    }

    println!("────────────────────────────────────────────────────────────────────");
    println!("  60 s complete.  Final state: {:?}", state);
    println!("  Review the trace above for state transitions and events.");

    assert_eq!(
        polls_completed, TOTAL_POLLS,
        "expected {TOTAL_POLLS} polls, got {polls_completed}"
    );
}
