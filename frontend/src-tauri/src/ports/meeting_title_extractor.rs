use crate::ports::meeting_detector::BrowserWindow;

/// Best-effort vendor-neutral meeting-title extractor.
///
/// Runs AFTER detection as decoration — it never gates the entry signal. Returns
/// the active call's window title, or `None` to fall through to the generic
/// timestamp default. Each vendor ships an adapter carrying its own title regex;
/// the wired adapter is called per window at each priority step of
/// `resolve_default_title`.
pub trait MeetingTitleExtractorPort: Send + Sync {
    fn extract_title(&self, windows: &[BrowserWindow]) -> Option<String>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::meeting_detector::BrowserWindow;

    struct StubExtractor {
        title: Option<String>,
    }

    impl MeetingTitleExtractorPort for StubExtractor {
        fn extract_title(&self, windows: &[BrowserWindow]) -> Option<String> {
            if windows.is_empty() {
                return None;
            }
            self.title.clone()
        }
    }

    #[test]
    fn stub_returns_configured_value_for_a_slice() {
        let win = BrowserWindow { hwnd_id: 1, pid: 42, title: "x".to_string() };
        let some = StubExtractor { title: Some("opv-augt-jbm".to_string()) };
        let none = StubExtractor { title: None };
        assert_eq!(some.extract_title(&[win.clone()]).as_deref(), Some("opv-augt-jbm"));
        assert!(none.extract_title(&[win]).is_none());
    }

    #[test]
    fn empty_slice_returns_none() {
        let extractor = StubExtractor { title: Some("never".to_string()) };
        assert!(extractor.extract_title(&[]).is_none());
    }
}
