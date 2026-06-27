//! Meet title-extractor adapter — best-effort meeting-title decoration.
//! Extracted from `detection/windows.rs`. Carries the EN-dash (U+2013) fix that
//! was the original scope of this change: the PWA format observed in the wild is
//! `Google Meet - Meet – opv-augt-jbm` (EN dash), not the EM dash (`—`) the old
//! regex expected — the mismatch silently disabled all PWA title extraction.
//! Demoted from load-bearing detection regex to best-effort decorator: detection
//! no longer consults the title, so a future title-format change cannot disable
//! detection the way the dash bug did.

use regex::Regex;

use crate::ports::meeting_detector::BrowserWindow;
use crate::ports::meeting_title_extractor::MeetingTitleExtractorPort;

/// Defensive upper bound on title length. A real conference window title is at
/// most a few hundred chars (Win32 `GetWindowTextW` truncates at 512 code units);
/// anything longer is synthetic or hostile and is rejected before the regex runs
/// (CLAUDE.md §9 boundary check).
const MAX_TITLE_LEN: usize = 1024;

pub struct MeetTitleExtractor;

impl MeetTitleExtractor {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MeetTitleExtractor {
    fn default() -> Self {
        Self::new()
    }
}

impl MeetingTitleExtractorPort for MeetTitleExtractor {
    fn extract_title(&self, windows: &[BrowserWindow]) -> Option<String> {
        for win in windows {
            if win.title.len() > MAX_TITLE_LEN {
                continue;
            }
            if !is_maybe_meet_title(&win.title) {
                continue;
            }
            if meet_title_regex().is_match(&win.title) {
                return Some(strip_google_meet_suffix(&win.title));
            }
        }
        None
    }
}

/// Cheap prefix/suffix guard to avoid regex overhead for the common non-Meet case.
fn is_maybe_meet_title(title: &str) -> bool {
    title.starts_with("Meet - ")
        || title.starts_with("Meet \u{2013} ")
        || title.starts_with("Google Meet - Meet ")
        || title.ends_with(" - Google Meet")
}

/// Matches the Meet title formats observed in the wild:
///   Chrome/Edge tab:   `Meet - <name>`
///   Edge tab-group:    `Meet – <code> and N more pages …`
///   PWA:               `Google Meet - Meet – <name>` (EN dash U+2013)
///   Suffix:            `<name> - Google Meet`
fn meet_title_regex() -> &'static Regex {
    static RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(
            r"^Meet - .+|^Meet \u{2013} .+|^Google Meet - Meet \u{2013} .+|.+ - Google Meet$",
        )
        .expect("meet title regex is valid")
    })
}

/// Strips the Meet framing from a matched title, returning the bare meeting name.
fn strip_google_meet_suffix(title: &str) -> String {
    if let Some(name) = title.strip_prefix("Meet - ") {
        name.trim().to_string()
    } else if title.starts_with("Google Meet - Meet ") {
        title
            .split('\u{2013}') // EN dash — the PWA format observed in the wild
            .nth(1)
            .map(|s| s.trim().to_string())
            .unwrap_or_else(|| title.to_string())
    } else if let Some(rest) = title.strip_prefix("Meet \u{2013} ") {
        // Edge tab-group collapsed: "Meet – <code> and N more pages - <group> - Microsoft Edge"
        rest.split_once(" and ")
            .map(|(code, _)| code)
            .unwrap_or(rest)
            .trim()
            .to_string()
    } else if let Some(name) = title.strip_suffix(" - Google Meet") {
        name.trim().to_string()
    } else {
        title.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::meeting_detector::BrowserWindow;

    fn win(title: &str) -> BrowserWindow {
        BrowserWindow {
            hwnd_id: 1,
            pid: 100,
            title: title.to_string(),
        }
    }

    fn extract(title: &str) -> Option<String> {
        MeetTitleExtractor.extract_title(&[win(title)])
    }

    // ── Chrome/Edge tab format: "Meet - <name>" ────────────────────────────

    #[test]
    fn chrome_tab_format_matches() {
        assert_eq!(extract("Meet - Weekly sync").as_deref(), Some("Weekly sync"));
        assert_eq!(extract("Meet - abc-defg-hij").as_deref(), Some("abc-defg-hij"));
        assert!(extract("Meet - ").is_none(), "nothing after prefix — no match");
    }

    // ── §3.1 PWA format: EN dash (the fix) ─────────────────────────────────

    #[test]
    fn pwa_format_uses_en_dash() {
        assert_eq!(
            extract("Google Meet - Meet \u{2013} opv-augt-jbm").as_deref(),
            Some("opv-augt-jbm")
        );
        assert_eq!(
            extract("Google Meet - Meet \u{2013} Weekly Sync").as_deref(),
            Some("Weekly Sync")
        );
    }

    // ── §3.2 adversarial: non-Latin / emoji / green-room title ─────────────

    #[test]
    fn green_room_emoji_parenthesis_title() {
        assert_eq!(
            extract("Google Meet - Meet \u{2013} 🎡 Search XP Playground (new)").as_deref(),
            Some("🎡 Search XP Playground (new)")
        );
    }

    #[test]
    fn unicode_emoji_tab_title() {
        assert_eq!(extract("Meet - 📊 Q4 review").as_deref(), Some("📊 Q4 review"));
        assert!(extract("Meet - مراجعة Q4").is_some());
    }

    // ── §3.3 suffix strip (PWA) ─────────────────────────────────────────────

    #[test]
    fn strip_pwa_en_dash_suffix() {
        assert_eq!(
            strip_google_meet_suffix("Google Meet - Meet \u{2013} opv-augt-jbm"),
            "opv-augt-jbm"
        );
    }

    // ── §3.4 adversarial: EM-dash variant does NOT match ───────────────────

    #[test]
    fn em_dash_variant_does_not_match() {
        assert!(
            extract("Google Meet - Meet \u{2014} Test").is_none(),
            "the fix must not silently accept both dash types"
        );
    }

    // ── Suffix format: "<Name> - Google Meet" ─────────────────────────────

    #[test]
    fn suffix_format_matches() {
        assert_eq!(
            extract("Sprint planning - Google Meet").as_deref(),
            Some("Sprint planning")
        );
        assert!(extract("Google Meet").is_none(), "lobby / no meeting name — must not match");
    }

    // ── Edge tab-group collapsed format ────────────────────────────────────

    #[test]
    fn edge_tabgroup_format_matches() {
        assert_eq!(
            extract("Meet \u{2013} add-acfj-djw and 19 more pages - Work - Microsoft\u{200b} Edge")
                .as_deref(),
            Some("add-acfj-djw")
        );
        assert_eq!(extract("Meet \u{2013} abc-defg-hij").as_deref(), Some("abc-defg-hij"));
    }

    // ── §3.5 boundary ──────────────────────────────────────────────────────

    #[test]
    fn empty_slice_returns_none() {
        assert!(MeetTitleExtractor.extract_title(&[]).is_none());
    }

    #[test]
    fn all_non_meet_slice_returns_none() {
        // D4.2 cross-vendor mitigation: no wired vendor matches a non-empty slice.
        let wins = [
            win("Gmail - Inbox"),
            win("Zoom Meeting"),
        ];
        assert!(MeetTitleExtractor.extract_title(&wins).is_none());
    }

    #[test]
    fn oversized_title_returns_none() {
        let long_title = format!("Google Meet - Meet \u{2013} {}", "x".repeat(2000));
        assert!(extract(&long_title).is_none());
    }

    // ── Non-Meet titles ────────────────────────────────────────────────────

    #[test]
    fn non_meet_titles_do_not_match() {
        assert!(extract("Chat with team about Google Meet").is_none());
        assert!(extract("Sprint planning - YouTube").is_none());
        assert!(extract("Zoom Meeting").is_none());
    }

    // ── §3.6 adversarial: injection titles pass through as opaque text ─────
    // Downstream `sanitize_filename` + parameterized sqlx mitigate at the
    // boundary; this test just preserves the security coverage through the move.

    #[test]
    fn injection_titles_pass_through_opaque() {
        assert_eq!(
            extract("Meet - '; DROP TABLE meetings; --").as_deref(),
            Some("'; DROP TABLE meetings; --")
        );
        assert_eq!(
            extract("Meet - ../../etc/passwd").as_deref(),
            Some("../../etc/passwd")
        );
    }

    // ── §4 "garbled output": repetitive non-Latin tokens pass through ──────
    // The title-extractor analog of Whisper's "hallucinated repetitive tokens"
    // adversarial category: a meeting name of pure CJK glyphs with no Latin
    // prefix. The extractor must treat the captured title as opaque — downstream
    // `sanitize_filename` handles filesystem safety — so the recording can still
    // be attributed even when the title is non-Latin end to end.

    #[test]
    fn repetitive_cjk_tokens_pass_through_opaque() {
        assert_eq!(
            extract("Meet - 漢字漢字漢字").as_deref(),
            Some("漢字漢字漢字")
        );
    }
}
