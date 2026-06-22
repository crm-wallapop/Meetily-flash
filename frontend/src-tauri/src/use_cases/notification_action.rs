//! Dispatch use case for `meetily://` deep-link activations coming off toast action
//! buttons. The composition root (lib.rs) subscribes to the deep-link event, feeds
//! the raw URI here, and routes the resolved outcome to the existing recording
//! command paths. This module is pure: no WinRT, no Tauri, no I/O.

pub use crate::ports::notification_action::RecordingStatePort;

/// The recording command implied by a deep-link URI, or `Rejected` for anything that
/// is not exactly `meetily://recording/{stop,continue}`. Carries no payload, so no
/// untrusted URI component can reach a command via this type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Stop,
    Continue,
    Rejected,
}

/// Whether a valid action should actually run given the current recording state.
/// `NoOp` covers the abnormal-activation cases the spec requires to be safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// Run the action's command in the composition root.
    Execute(Action),
    /// Valid URI but acting on it now would be wrong (cold-start stop, double-stop,
    /// continue while already recording). Log and skip.
    NoOp,
}

/// Parse and validate a deep-link URI. Deep-link URIs are attacker-controllable
/// external input (design Decision 4): only `scheme == meetily`, `host == recording`,
/// and `action ∈ {stop, continue}` are accepted, with a single path segment and no
/// port/userinfo/fragment. Scheme and host are matched case-insensitively (RFC 3986);
/// the action verb is matched exactly lowercase because our generated URIs are
/// lowercase and case variation here is not a legitimate signal. Unknown query
/// parameters are dropped without inspection — the `Action` return type carries no
/// data, so nothing untrusted can propagate.
pub fn dispatch_notification_action(uri: &str) -> Action {
    let Some(scheme_end) = uri.find("://") else {
        return Action::Rejected;
    };
    let scheme = &uri[..scheme_end];
    if !scheme.eq_ignore_ascii_case("meetily") {
        return Action::Rejected;
    }

    let after_scheme = &uri[scheme_end + 3..];

    // Fragments are never produced by our toast URIs; treat one as malformed. Checked
    // before query stripping so `stop?x=1#frag` is rejected, not silently swallowed with
    // the discarded query.
    if after_scheme.contains('#') {
        return Action::Rejected;
    }

    // Split off any query before authority/path validation. Query is never read — its
    // presence is legal, its contents are dropped.
    let authority_and_path = match after_scheme.find('?') {
        Some(i) => &after_scheme[..i],
        None => after_scheme,
    };

    let Some((host, path)) = authority_and_path.split_once('/') else {
        return Action::Rejected;
    };

    // Reject userinfo (`user@recording`) and explicit ports (`recording:8080`) —
    // neither appears in our URIs and both are injection vectors at the authority.
    if host.contains('@') || host.contains(':') || !host.eq_ignore_ascii_case("recording") {
        return Action::Rejected;
    }

    // Exactly one path segment with no trailing slash. `stop/` or `stop/extra` reject.
    match path {
        "stop" => Action::Stop,
        "continue" => Action::Continue,
        _ => Action::Rejected,
    }
}

/// Guard a resolved action against the live recording state. `Stop` when nothing is
/// recording (cold start, or a second tap after the first already stopped) is a no-op;
/// `Continue` while already recording is a no-op. `Rejected` is a no-op (the
/// composition root logs and skips before calling this, but the arm is total).
pub fn resolve(action: Action, state: &dyn RecordingStatePort) -> Resolved {
    match action {
        Action::Stop => {
            if state.is_recording() {
                Resolved::Execute(Action::Stop)
            } else {
                Resolved::NoOp
            }
        }
        Action::Continue => {
            if state.is_recording() {
                Resolved::NoOp
            } else {
                Resolved::Execute(Action::Continue)
            }
        }
        Action::Rejected => Resolved::NoOp,
    }
}

/// Pull the first `meetily://` URI out of a process-argv slice. The composition root
/// calls this from the single-instance plugin callback: a toast-button re-activation
/// re-launches the app with the URI as argv, and single-instance forwards it to the
/// *running* instance instead of booting a second one. The scheme match is
/// case-insensitive (the OS scheme key is); every non-meetily arg is ignored.
/// `dispatch_notification_action` re-validates host and action, so a permissive scheme
/// filter here cannot let an untrusted payload reach a command.
pub fn extract_meetily_uri(argv: &[String]) -> Option<&str> {
    argv.iter().find_map(|a| {
        let b = a.as_bytes();
        if b.len() >= 10 && b[..8].eq_ignore_ascii_case(b"meetily:") && &b[8..10] == b"//" {
            Some(a.as_str())
        } else {
            None
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeState(bool);
    impl RecordingStatePort for FakeState {
        fn is_recording(&self) -> bool {
            self.0
        }
    }

    const RECORDING: FakeState = FakeState(true);
    const IDLE: FakeState = FakeState(false);

    // --- Happy path: the two URIs our toast buttons emit ---

    #[test]
    fn accepts_stop_uri() {
        assert_eq!(dispatch_notification_action("meetily://recording/stop"), Action::Stop);
    }

    #[test]
    fn accepts_continue_uri() {
        assert_eq!(
            dispatch_notification_action("meetily://recording/continue"),
            Action::Continue
        );
    }

    // --- Adversarial: unknown action / wrong scheme / wrong host ---

    #[test]
    fn rejects_unknown_action() {
        assert_eq!(
            dispatch_notification_action("meetily://recording/pause"),
            Action::Rejected
        );
    }

    #[test]
    fn rejects_wrong_scheme() {
        assert_eq!(
            dispatch_notification_action("https://recording/stop"),
            Action::Rejected
        );
        assert_eq!(
            dispatch_notification_action("meetily-recording/stop"),
            Action::Rejected
        );
    }

    #[test]
    fn rejects_wrong_host() {
        // Simulates a crafted or colliding URI targeting a different host.
        assert_eq!(
            dispatch_notification_action("meetily://malicious/stop"),
            Action::Rejected
        );
        assert_eq!(
            dispatch_notification_action("meetily://recordings/stop"),
            Action::Rejected
        );
    }

    // --- Adversarial: unknown query parameters are ignored, not propagated ---

    #[test]
    fn unknown_query_params_are_ignored() {
        // The action still resolves; the Action type carries no data, so the
        // untrusted `extra=evil` value has nowhere to go.
        assert_eq!(
            dispatch_notification_action("meetily://recording/stop?extra=evil"),
            Action::Stop
        );
        assert_eq!(
            dispatch_notification_action("meetily://recording/continue?a=1&b=2"),
            Action::Continue
        );
        // A query that itself contains path-like or scheme-like text must not confuse
        // the authority/path split.
        assert_eq!(
            dispatch_notification_action("meetily://recording/stop?u=https://evil/x"),
            Action::Stop
        );
    }

    #[test]
    fn rejects_fragment_after_query() {
        // A fragment after a query is rejected, not silently swallowed with the
        // discarded query — defence in depth against any future code that reads the
        // raw URI before this function trims it.
        assert_eq!(
            dispatch_notification_action("meetily://recording/stop?x=1#frag"),
            Action::Rejected
        );
        assert_eq!(
            dispatch_notification_action("meetily://recording/continue?a=1#"),
            Action::Rejected
        );
    }

    // --- Adversarial: malformed URIs ---

    #[test]
    fn rejects_malformed_uris() {
        let bad = [
            "",
            "not a url",
            "meetily:",
            "meetily//recording/stop",     // missing colon
            "meetily://",                   // nothing after scheme
            "meetily://recording",          // no action
            "meetily://recording/",         // empty action
            "meetily://recording/stop/",    // trailing slash
            "meetily://recording/stop/extra", // second path segment
            "meetily://recording/stop#frag",  // fragment
            "meetily://recording:8080/stop",  // explicit port
            "meetily://user@recording/stop",  // userinfo
            "meetily:///stop",              // empty host
            "  meetily://recording/stop",   // leading whitespace
            "meetily://recording/stop\n",   // trailing newline
            "MEETILY://recording/stop\t",   // trailing tab
        ];
        for uri in bad {
            assert_eq!(
                dispatch_notification_action(uri),
                Action::Rejected,
                "expected {uri:?} to be rejected"
            );
        }
    }

    // --- RFC 3986: scheme/host case-insensitive, action verb exact ---

    #[test]
    fn scheme_and_host_case_insensitive_but_action_exact() {
        assert_eq!(
            dispatch_notification_action("Meetily://Recording/stop"),
            Action::Stop
        );
        // A capitalised action verb is not one of our buttons.
        assert_eq!(
            dispatch_notification_action("meetily://recording/Stop"),
            Action::Rejected
        );
        assert_eq!(
            dispatch_notification_action("meetily://recording/CONTINUE"),
            Action::Rejected
        );
    }

    // --- Abnormal-activation guards (spec: cold-start, double-tap, continue-while-recording) ---

    #[test]
    fn cold_start_stop_is_noop() {
        // App not recording → a stop button tap must do nothing.
        assert_eq!(resolve(Action::Stop, &IDLE), Resolved::NoOp);
    }

    #[test]
    fn double_stop_is_idempotent() {
        // First stop runs (recording → Execute). After it, state is idle, so a second
        // stop is the same NoOp as the cold-start case — idempotent by construction.
        assert_eq!(resolve(Action::Stop, &RECORDING), Resolved::Execute(Action::Stop));
        assert_eq!(resolve(Action::Stop, &IDLE), Resolved::NoOp);
    }

    #[test]
    fn continue_while_recording_is_noop() {
        assert_eq!(resolve(Action::Continue, &RECORDING), Resolved::NoOp);
    }

    #[test]
    fn continue_when_idle_executes() {
        // Stopped-toast [Continue recording] starts a fresh capture.
        assert_eq!(resolve(Action::Continue, &IDLE), Resolved::Execute(Action::Continue));
    }

    #[test]
    fn stop_while_recording_executes() {
        assert_eq!(resolve(Action::Stop, &RECORDING), Resolved::Execute(Action::Stop));
    }

    #[test]
    fn rejected_action_is_noop_under_resolve() {
        // The composition root skips before calling resolve, but the arm is total.
        assert_eq!(resolve(Action::Rejected, &RECORDING), Resolved::NoOp);
        assert_eq!(resolve(Action::Rejected, &IDLE), Resolved::NoOp);
    }

    // --- extract_meetily_uri: single-instance argv forwarding (pure) ---

    fn argv(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn extract_finds_meetily_uri() {
        let a = argv(&["meetily-flash.exe", "meetily://recording/stop"]);
        assert_eq!(extract_meetily_uri(&a), Some("meetily://recording/stop"));
    }

    #[test]
    fn extract_none_when_no_meetily_uri() {
        assert_eq!(extract_meetily_uri(&argv(&["meetily-flash.exe", "--flag"])), None);
        assert_eq!(extract_meetily_uri(&argv(&[])), None);
    }

    #[test]
    fn extract_finds_uri_in_any_position() {
        // Real invocations put the exe path first; defend any argv position.
        let a = argv(&["C:\\mf\\debug\\meetily-flash.exe", "--foo", "meetily://recording/continue"]);
        assert_eq!(extract_meetily_uri(&a), Some("meetily://recording/continue"));
    }

    #[test]
    fn extract_returns_first_when_multiple() {
        let a = argv(&["meetily://recording/stop", "meetily://recording/continue"]);
        assert_eq!(extract_meetily_uri(&a), Some("meetily://recording/stop"));
    }

    #[test]
    fn extract_ignores_lookalikes() {
        assert_eq!(extract_meetily_uri(&argv(&["https://meetily://x"])), None);
        assert_eq!(extract_meetily_uri(&argv(&["meetily-recording/stop"])), None);
        assert_eq!(extract_meetily_uri(&argv(&["meetily:recording/stop"])), None);
    }

    #[test]
    fn extract_matches_scheme_case_insensitively() {
        assert_eq!(
            extract_meetily_uri(&argv(&["Meetily://recording/stop"])),
            Some("Meetily://recording/stop")
        );
    }
}
