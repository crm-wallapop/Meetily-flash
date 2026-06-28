## ADDED Requirements

### Requirement: Stop-completion toast action is conditional on a known meeting id

The in-app recording-stop completion toast SHALL render its "View Meeting" navigation action only when a `meetingId` is known at toast-render time. When `meetingId` is unknown (null or undefined), the app SHALL omit the action entirely (the success message MAY still appear) and SHALL NOT render a clickable control whose handler silently no-ops.

#### Scenario: Stop with known meeting id renders a working View Meeting action
- **GIVEN** a recording stops AND at least one of `stopResult.meeting_id` or `activeMeetingId` is non-null
- **WHEN** the completion toast renders
- **THEN** the "View Meeting" action is shown
- **AND** clicking it navigates to `/meeting-details?id=<meetingId>`

#### Scenario: Stop with unknown meeting id omits the action
- **GIVEN** a recording stops AND both `stopResult.meeting_id` and `activeMeetingId` are null
- **WHEN** the completion toast renders
- **THEN** no "View Meeting" action is rendered, though the success message MAY still appear
- **AND** no clickable control exists whose handler would silently no-op
