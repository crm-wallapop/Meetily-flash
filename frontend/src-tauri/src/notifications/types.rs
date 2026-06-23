use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notification {
    pub id: Option<String>,
    pub title: String,
    pub body: String,
    pub notification_type: NotificationType,
    pub priority: NotificationPriority,
    pub timeout: NotificationTimeout,
    pub icon: Option<String>,
    pub sound: bool,
    pub actions: Vec<NotificationAction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum NotificationType {
    RecordingStarted,
    /// Detector-triggered record start. Gated by the same consent/preference as
    /// `RecordingStarted` but carries the "Meeting detected" wording so the user can
    /// tell an auto-started recording from a manual one.
    MeetingDetected,
    RecordingStopped,
    RecordingPaused,
    RecordingResumed,
    TranscriptionComplete,
    MeetingReminder(u64), // Duration in minutes
    SystemError(String),
    Test, // For testing notifications
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationPriority {
    Low,
    Normal,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationTimeout {
    Never,
    Seconds(u64),
    Default,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationAction {
    pub id: String,
    pub title: String,
    pub action_type: NotificationActionType,
    /// Protocol-launch URI for `activationType="protocol"` toast buttons
    /// (e.g. `meetily://recording/stop`). `None` means default dismissal — the
    /// toast closes and no command runs.
    #[serde(default)]
    pub launch_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationActionType {
    Button,
    Reply,
}

impl NotificationAction {
    pub fn button(id: impl Into<String>, title: impl Into<String>, launch_uri: Option<String>) -> Self {
        Self {
            id: id.into(),
            title: title.into(),
            action_type: NotificationActionType::Button,
            launch_uri,
        }
    }
}

/// Whether a recording was started by the auto-detect feature or by the user. Drives
/// the record-start toast wording so the two paths are distinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordStartSource {
    Detector,
    Manual,
}

/// Fixed deep-link URIs emitted by the toast action buttons. These are the only
/// values the dispatch use case accepts (see `use_cases::notification_action`); the
/// adapter renders them verbatim and the composition root validates them on arrival.
pub const URI_STOP: &str = "meetily://recording/stop";
pub const URI_CONTINUE: &str = "meetily://recording/continue";

/// Body text for the record-start toast, parameterised by source per the
/// notification-actions spec. Pure so the wording can be unit-tested without the
/// notification subsystem.
pub fn recording_started_body(source: RecordStartSource, title: Option<&str>) -> String {
    match source {
        RecordStartSource::Detector => match title {
            Some(t) => format!("Meeting detected — recording: {}", t),
            None => "Meeting detected — recording".to_string(),
        },
        RecordStartSource::Manual => match title {
            Some(t) => format!("Recording started: {}", t),
            None => "Recording has started".to_string(),
        },
    }
}

/// Body text for the recording-stopped toast. Pure so the wording can be shared
/// between the manager path and the fallback (Tauri-direct) path without drift.
/// An empty-string title collapses to the generic body so a caller passing
/// `Some("")` doesn't render "Recording saved: " with a dangling colon.
pub fn recording_stopped_body(meeting_name: Option<&str>) -> String {
    match meeting_name {
        Some(name) if !name.is_empty() => format!("Recording saved: {}", name),
        _ => "Recording saved".to_string(),
    }
}

/// Action set for the recording-active toast (started or detected): stop-and-save,
/// or keep running. Order matches the spec (`[Stop recording]` then `[Continue]`).
pub fn recording_active_actions() -> Vec<NotificationAction> {
    vec![
        NotificationAction::button("stop", "Stop recording", Some(URI_STOP.to_string())),
        NotificationAction::button("continue", "Continue", Some(URI_CONTINUE.to_string())),
    ]
}

/// Action set for the recording-stopped toast: resume capture as a fresh session,
/// or accept the stop. `[Dismiss]` has no launch URI — it is default dismissal.
pub fn recording_stopped_actions() -> Vec<NotificationAction> {
    vec![
        NotificationAction::button("continue", "Continue recording", Some(URI_CONTINUE.to_string())),
        NotificationAction::button("dismiss", "Dismiss", None),
    ]
}

impl Notification {
    pub fn new(title: impl Into<String>, body: impl Into<String>, notification_type: NotificationType) -> Self {
        Self {
            id: None,
            title: title.into(),
            body: body.into(),
            notification_type,
            priority: NotificationPriority::Normal,
            timeout: NotificationTimeout::Default,
            icon: None,
            sound: true,
            actions: vec![],
        }
    }

    pub fn with_priority(mut self, priority: NotificationPriority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_timeout(mut self, timeout: NotificationTimeout) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_sound(mut self, sound: bool) -> Self {
        self.sound = sound;
        self
    }

    pub fn with_icon(mut self, icon: impl Into<String>) -> Self {
        self.icon = Some(icon.into());
        self
    }

    pub fn with_id(mut self, id: impl Into<String>) -> Self {
        self.id = Some(id.into());
        self
    }

    pub fn add_action(mut self, action: NotificationAction) -> Self {
        self.actions.push(action);
        self
    }
}

impl Default for NotificationPriority {
    fn default() -> Self {
        NotificationPriority::Normal
    }
}

impl Default for NotificationTimeout {
    fn default() -> Self {
        NotificationTimeout::Default
    }
}

// Helper functions for creating common notifications
impl Notification {
    /// Manual record-start toast: "Recording started: \<title\>" + `[Stop recording]` / `[Continue]`.
    pub fn recording_started(meeting_name: Option<String>) -> Self {
        let body = recording_started_body(RecordStartSource::Manual, meeting_name.as_deref());
        Notification::new("Meetily", body, NotificationType::RecordingStarted)
            .with_priority(NotificationPriority::High)
            .with_timeout(NotificationTimeout::Seconds(5))
            .add_action(NotificationAction::button("stop", "Stop recording", Some(URI_STOP.to_string())))
            .add_action(NotificationAction::button("continue", "Continue", Some(URI_CONTINUE.to_string())))
    }

    /// Detector-triggered record-start toast: "Meeting detected — recording: \<title\>" + the
    /// same two buttons.
    pub fn recording_detected(meeting_name: Option<String>) -> Self {
        let body = recording_started_body(RecordStartSource::Detector, meeting_name.as_deref());
        Notification::new("Meetily", body, NotificationType::MeetingDetected)
            .with_priority(NotificationPriority::High)
            .with_timeout(NotificationTimeout::Seconds(5))
            .add_action(NotificationAction::button("stop", "Stop recording", Some(URI_STOP.to_string())))
            .add_action(NotificationAction::button("continue", "Continue", Some(URI_CONTINUE.to_string())))
    }

    /// Recording-stopped toast: "Recording saved: \<title\>" + `[Continue recording]` / `[Dismiss]`.
    /// The title names the saved meeting so the user can tell which session was persisted.
    pub fn recording_stopped(meeting_name: Option<String>) -> Self {
        let body = recording_stopped_body(meeting_name.as_deref());
        Notification::new("Meetily", body, NotificationType::RecordingStopped)
            .with_priority(NotificationPriority::Normal)
            .with_timeout(NotificationTimeout::Seconds(3))
            .add_action(NotificationAction::button(
                "continue",
                "Continue recording",
                Some(URI_CONTINUE.to_string()),
            ))
            .add_action(NotificationAction::button("dismiss", "Dismiss", None))
    }

    pub fn recording_paused() -> Self {
        Notification::new(
            "Meetily",
            "Recording has been paused",
            NotificationType::RecordingPaused
        )
        .with_priority(NotificationPriority::Normal)
        .with_timeout(NotificationTimeout::Seconds(3))
    }

    pub fn recording_resumed() -> Self {
        Notification::new(
            "Meetily",
            "Recording has been resumed",
            NotificationType::RecordingResumed
        )
        .with_priority(NotificationPriority::Normal)
        .with_timeout(NotificationTimeout::Seconds(3))
    }

    pub fn transcription_complete(file_path: Option<String>) -> Self {
        let body = match file_path {
            Some(path) => format!("Transcription completed and saved to: {}", path),
            None => "Transcription has been completed".to_string(),
        };

        Notification::new("Meetily", body, NotificationType::TranscriptionComplete)
            .with_priority(NotificationPriority::Normal)
            .with_timeout(NotificationTimeout::Seconds(5))
    }

    pub fn meeting_reminder(minutes_until: u64, meeting_title: Option<String>) -> Self {
        let body = match meeting_title {
            Some(title) => format!("Meeting '{}' starts in {} minutes", title, minutes_until),
            None => format!("Meeting starts in {} minutes", minutes_until),
        };

        Notification::new("Meetily", body, NotificationType::MeetingReminder(minutes_until))
            .with_priority(NotificationPriority::High)
            .with_timeout(NotificationTimeout::Seconds(10))
    }

    pub fn system_error(error: impl Into<String>) -> Self {
        let error_string = error.into();
        Notification::new(
            "Meetily Error",
            error_string.clone(),
            NotificationType::SystemError(error_string)
        )
        .with_priority(NotificationPriority::Critical)
        .with_timeout(NotificationTimeout::Never)
    }

    pub fn test_notification() -> Self {
        Notification::new(
            "Meetily",
            "This is a test notification to verify the system is working correctly",
            NotificationType::Test
        )
        .with_priority(NotificationPriority::Normal)
        .with_timeout(NotificationTimeout::Seconds(5))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_start_wording_names_the_title() {
        assert_eq!(
            recording_started_body(RecordStartSource::Manual, Some("Standup")),
            "Recording started: Standup"
        );
        assert_eq!(
            recording_started_body(RecordStartSource::Manual, None),
            "Recording has started"
        );
    }

    #[test]
    fn detector_start_wording_calls_out_detection() {
        assert_eq!(
            recording_started_body(RecordStartSource::Detector, Some("Standup")),
            "Meeting detected — recording: Standup"
        );
        assert_eq!(
            recording_started_body(RecordStartSource::Detector, None),
            "Meeting detected — recording"
        );
    }

    #[test]
    fn recording_started_carries_stop_and_continue_actions() {
        let n = Notification::recording_started(Some("Sync".into()));
        assert_eq!(n.notification_type, NotificationType::RecordingStarted);
        let uris: Vec<_> = n.actions.iter().map(|a| a.launch_uri.as_deref()).collect();
        assert_eq!(uris, vec![Some(URI_STOP), Some(URI_CONTINUE)]);
        assert_eq!(n.actions[0].title, "Stop recording");
        assert_eq!(n.actions[1].title, "Continue");
    }

    #[test]
    fn recording_detected_uses_detector_wording_and_type() {
        let n = Notification::recording_detected(Some("Sync".into()));
        assert_eq!(n.notification_type, NotificationType::MeetingDetected);
        assert!(n.body.starts_with("Meeting detected — recording:"));
        // Same action set as a manual start.
        let uris: Vec<_> = n.actions.iter().map(|a| a.launch_uri.as_deref()).collect();
        assert_eq!(uris, vec![Some(URI_STOP), Some(URI_CONTINUE)]);
    }

    #[test]
    fn recording_stopped_carries_continue_and_dismiss() {
        let n = Notification::recording_stopped(Some("Standup".into()));
        assert_eq!(n.body, "Recording saved: Standup");
        assert_eq!(n.actions.len(), 2);
        assert_eq!(n.actions[0].title, "Continue recording");
        assert_eq!(n.actions[0].launch_uri.as_deref(), Some(URI_CONTINUE));
        // Dismiss is default dismissal: no launch URI.
        assert_eq!(n.actions[1].title, "Dismiss");
        assert!(n.actions[1].launch_uri.is_none());
    }

    #[test]
    fn recording_stopped_without_title_uses_generic_body() {
        let n = Notification::recording_stopped(None);
        assert_eq!(n.body, "Recording saved");
    }

    #[test]
    fn recording_stopped_body_collapses_empty_title_to_generic() {
        // Boundary input: Some("") must not render "Recording saved: " with a
        // dangling colon — it collapses to the generic body.
        assert_eq!(recording_stopped_body(None), "Recording saved");
        assert_eq!(recording_stopped_body(Some("")), "Recording saved");
        assert_eq!(
            recording_stopped_body(Some("Standup")),
            "Recording saved: Standup"
        );
    }

    #[test]
    fn action_helpers_emit_only_whitelisted_uris() {
        for a in recording_active_actions().iter().chain(recording_stopped_actions().iter()) {
            match a.launch_uri.as_deref() {
                None | Some(URI_STOP) | Some(URI_CONTINUE) => {}
                other => panic!("non-whitelisted launch URI: {other:?}"),
            }
        }
    }
}
