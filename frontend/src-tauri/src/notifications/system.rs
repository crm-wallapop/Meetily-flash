use crate::notifications::types::{Notification, NotificationPriority, NotificationTimeout};
use anyhow::{Result, anyhow};
use log::{info as log_info, error as log_error};
use tauri::{AppHandle, Runtime};
use tauri_plugin_notification::NotificationExt;
use std::time::Duration;

/// Cross-platform system notification handler
pub struct SystemNotificationHandler<R: Runtime> {
    app_handle: AppHandle<R>,
}

impl<R: Runtime> SystemNotificationHandler<R> {
    pub fn new(app_handle: AppHandle<R>) -> Self {
        Self {
            app_handle,
        }
    }

    /// Show a notification using Tauri's notification plugin
    pub async fn show_notification(&self, notification: Notification) -> Result<()> {
        log_info!("Attempting to show notification: {}", notification.title);

        // Check if DND is active and respect user settings
        if self.is_dnd_active().await && self.should_respect_dnd(&notification) {
            log_info!("DND is active, skipping notification: {}", notification.title);
            return Ok(());
        }

        // Action-bearing toasts need raw WinRT XML with protocol-activation buttons;
        // tauri-plugin-notification's builder can only render title+body. Windows-only —
        // other platforms fall through to the actionless path (macOS actions are a
        // separate future change, per design).
        #[cfg(target_os = "windows")]
        if !notification.actions.is_empty() {
            let aumid = self.app_handle.config().identifier.clone();
            return show_action_toast(
                &notification.title,
                &notification.body,
                &notification.actions,
                &aumid,
            );
        }

        // Use Tauri notification for all platforms
        log_info!("Showing Tauri notification: {}", notification.title);

        let builder = self.app_handle.notification().builder()
            .title(&notification.title)
            .body(&notification.body);

        match builder.show() {
            Ok(_) => {
                log_info!("Successfully showed Tauri notification: {}", notification.title);
                Ok(())
            }
            Err(e) => {
                log_error!("Failed to show Tauri notification: {}", e);
                Err(anyhow!("Failed to show notification: {}", e))
            }
        }
    }

    /// Check if Do Not Disturb is currently active
    /// Note: DND is managed through app settings, not system-level checks
    pub async fn is_dnd_active(&self) -> bool {
        // App manages DND through its own notification settings
        // No need to check system-level DND status
        false
    }

    /// Get the actual system DND status
    /// Note: DND is managed through app settings, not system-level checks
    pub async fn get_system_dnd_status(&self) -> bool {
        // App manages DND through its own notification settings
        // No need to check system-level DND status
        false
    }

    /// Request notification permission from the system
    pub async fn request_permission(&self) -> Result<bool> {
        log_info!("Requesting notification permission");

        // On most platforms with Tauri, permissions are handled automatically
        // We don't need to show a test notification during initialization
        log_info!("Notification permission granted (automatic for Tauri apps)");
        Ok(true)
    }

    /// Show a test notification to verify the system is working
    #[allow(dead_code)] // Used by show_test_notification command for manual testing
    async fn show_test_notification(&self) -> Result<()> {
        let test_notification = Notification::test_notification();
        self.show_notification(test_notification).await
    }

    /// Determine if we should respect DND for this notification
    fn should_respect_dnd(&self, notification: &Notification) -> bool {
        match notification.priority {
            NotificationPriority::Critical => false, // Always show critical notifications
            _ => true, // Respect DND for all other priorities
        }
    }

    /// Clear all notifications (platform-specific)
    pub async fn clear_notifications(&self) -> Result<()> {
        log_info!("Clearing all notifications");

        // This is platform-specific and complex to implement
        // For now, we'll just log that we attempted to clear
        // Future enhancement can add platform-specific clearing

        Ok(())
    }
}

/// Convert notification timeout to duration
impl From<&NotificationTimeout> for Option<Duration> {
    fn from(timeout: &NotificationTimeout) -> Self {
        match timeout {
            NotificationTimeout::Never => None,
            NotificationTimeout::Seconds(secs) => Some(Duration::from_secs(*secs)),
            NotificationTimeout::Default => Some(Duration::from_secs(5)),
        }
    }
}

/// Escape a string for safe interpolation into toast XML. Titles and bodies reach here
/// from untrusted boundary input (meeting names, window titles), so a stray `<` or `&`
/// must not be able to break out of the element or inject an action.
#[cfg(target_os = "windows")]
fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '\'' => out.push_str("&apos;"),
            '"' => out.push_str("&quot;"),
            _ => out.push(c),
        }
    }
    out
}

/// Build the ToastGeneric XML for an action toast. Pure (no I/O) so the escaping and
/// structure can be unit-tested independently of WinRT. Titles, bodies, and action labels
/// reach here from untrusted boundary input (meeting names, window titles); `xml_escape`
/// prevents element breakout or action injection. An empty `actions` slice omits the
/// `<actions>` block entirely (actionless toast).
#[cfg(target_os = "windows")]
pub(crate) fn build_action_toast_xml(
    title: &str,
    body: &str,
    actions: &[crate::notifications::types::NotificationAction],
) -> String {
    use std::fmt::Write;

    let mut actions_xml = String::new();
    if !actions.is_empty() {
        actions_xml.push_str("<actions>");
        for a in actions {
            match &a.launch_uri {
                Some(uri) => {
                    let _ = write!(
                        actions_xml,
                        "<action content='{}' activationType='protocol' arguments='{}'/>",
                        xml_escape(&a.title),
                        xml_escape(uri)
                    );
                }
                None => {
                    let _ = write!(
                        actions_xml,
                        "<action content='{}' activationType='system' arguments='dismiss'/>",
                        xml_escape(&a.title)
                    );
                }
            }
        }
        actions_xml.push_str("</actions>");
    }

    format!(
        "<toast><visual><binding template='ToastGeneric'>\
         <text>{}</text><text>{}</text></binding></visual>{}</toast>",
        xml_escape(title),
        xml_escape(body),
        actions_xml
    )
}

/// Build and show a Windows toast with protocol-activation action buttons via raw WinRT
/// XML. Buttons with a `launch_uri` activate the registered `meetily://` scheme, which
/// the deep-link plugin delivers to the running instance (or cold-starts it). Buttons
/// without a `launch_uri` use system dismissal. `aumid` is the AppUserModelID the toast
/// notifier is branded under (`com.meetily.ai`); it must have a DisplayName/IconUri
/// registry entry or Windows silently drops the toast.
#[cfg(target_os = "windows")]
pub fn show_action_toast(
    title: &str,
    body: &str,
    actions: &[crate::notifications::types::NotificationAction],
    aumid: &str,
) -> Result<()> {
    use windows::core::HSTRING;
    use windows::Data::Xml::Dom::XmlDocument;
    use windows::UI::Notifications::{ToastNotification, ToastNotificationManager};

    let xml = build_action_toast_xml(title, body, actions);

    let doc = XmlDocument::new()?;
    doc.LoadXml(&HSTRING::from(&xml))?;
    let toast = ToastNotification::CreateToastNotification(&doc)?;
    let notifier = ToastNotificationManager::CreateToastNotifierWithId(&HSTRING::from(aumid))?;
    notifier.Show(&toast)?;
    Ok(())
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    use crate::notifications::types::NotificationAction;

    #[test]
    fn build_xml_renders_protocol_and_dismiss_actions() {
        let actions = vec![
            NotificationAction::button("stop", "Stop recording", Some("meetily://recording/stop".into())),
            NotificationAction::button("dismiss", "Dismiss", None),
        ];
        let xml = build_action_toast_xml("Meetily", "Recording started", &actions);
        assert!(
            xml.contains("<action content='Stop recording' activationType='protocol' arguments='meetily://recording/stop'/>"),
            "protocol action missing or malformed: {xml}"
        );
        assert!(
            xml.contains("<action content='Dismiss' activationType='system' arguments='dismiss'/>"),
            "dismiss action missing or malformed: {xml}"
        );
        assert!(xml.contains("<text>Meetily</text><text>Recording started</text>"));
    }

    #[test]
    fn build_xml_escapes_untrusted_title_and_body() {
        // A meeting name with XML metacharacters must not break out of the <text>
        // element or inject an <action>. Body and title are escaped by the same
        // helper, but per the adversarial-TDD per-field rule each is probed for
        // every metacharacter independently — including `<` in the body.
        let xml = build_action_toast_xml(
            "Standup <script>alert(1)</script>",
            "Recording & \"meeting\" > done <img src=x>",
            &[],
        );
        assert!(!xml.contains("<script>"), "raw <script> leaked: {xml}");
        assert!(!xml.contains("<img"), "raw <img> leaked from body: {xml}");
        assert!(xml.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
        assert!(xml.contains("&lt;img src=x&gt;"));
        assert!(xml.contains("&amp;"));
        assert!(xml.contains("&quot;"));
        assert!(xml.contains("&gt;"));
        // No <actions> block for an empty action list.
        assert!(!xml.contains("<actions>"));
    }

    #[test]
    fn build_xml_escapes_action_label_and_uri() {
        // A crafted action label or URI must not escape its element or inject a new
        // action — the quotes and angle brackets are entity-encoded.
        let actions = vec![NotificationAction::button(
            "a",
            "Stop'/><action content='evil",
            Some("meetily://recording/stop'/><action".into()),
        )];
        let xml = build_action_toast_xml("T", "B", &actions);
        assert!(!xml.contains(">evil"), "injected action leaked: {xml}");
        assert!(xml.contains("&apos;"));
        assert!(xml.contains("&lt;action"));
    }

    #[test]
    fn build_xml_empty_actions_has_no_actions_block() {
        let xml = build_action_toast_xml("Meetily", "Body", &[]);
        assert!(!xml.contains("<actions>"));
        assert!(xml.contains("<toast>"));
        assert!(xml.contains("</toast>"));
    }

    #[test]
    fn build_xml_ampersand_in_uri_is_escaped() {
        let actions = vec![NotificationAction::button(
            "stop",
            "Stop",
            Some("meetily://recording/stop?x=1&y=2".into()),
        )];
        let xml = build_action_toast_xml("T", "B", &actions);
        assert!(xml.contains("x=1&amp;y=2"), "raw & in URI: {xml}");
        assert!(!xml.contains("x=1&y=2"));
    }
}
