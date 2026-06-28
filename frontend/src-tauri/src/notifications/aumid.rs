//! AUMID registry branding — ensures dev-build OS toasts display by populating
//! the AppUserModelId registry key with `DisplayName` + `IconUri` before any toast.
//! The pure decision logic is unit-tested here; the Windows registry I/O lives in
//! the `#[cfg(target_os = "windows")]` adapter below and is verified manually.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AumidState {
    pub display_name: Option<String>,
    pub icon_uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrandingAction {
    Write { display_name: String, icon_uri: String },
    NoOp,
}

/// Decide whether the AUMID needs branding, from its current registry state.
/// No I/O — the adapter reads the registry into `AumidState` and applies the
/// returned action. Writing both values when either differs or is absent keeps
/// the key consistent after a partial/corrupt prior branding.
pub fn ensure_aumid_branded(
    current: &AumidState,
    desired_display_name: &str,
    desired_icon_uri: &str,
) -> BrandingAction {
    match current {
        AumidState { display_name: Some(dn), icon_uri: Some(iu) }
            if dn == desired_display_name && iu == desired_icon_uri =>
        {
            BrandingAction::NoOp
        }
        _ => BrandingAction::Write {
            display_name: desired_display_name.to_string(),
            icon_uri: desired_icon_uri.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn both_missing_writes_both() {
        let state = AumidState::default();
        assert_eq!(
            ensure_aumid_branded(&state, "Meetily", "file:///x.ico"),
            BrandingAction::Write {
                display_name: "Meetily".to_string(),
                icon_uri: "file:///x.ico".to_string(),
            }
        );
    }

    #[test]
    fn both_present_and_matching_is_noop() {
        let state = AumidState {
            display_name: Some("Meetily".to_string()),
            icon_uri: Some("file:///x.ico".to_string()),
        };
        assert_eq!(
            ensure_aumid_branded(&state, "Meetily", "file:///x.ico"),
            BrandingAction::NoOp
        );
    }

    #[test]
    fn display_name_missing_writes_both() {
        let state = AumidState {
            display_name: None,
            icon_uri: Some("file:///x.ico".to_string()),
        };
        assert!(matches!(
            ensure_aumid_branded(&state, "Meetily", "file:///x.ico"),
            BrandingAction::Write { .. }
        ));
    }

    #[test]
    fn icon_uri_missing_writes_both() {
        let state = AumidState {
            display_name: Some("Meetily".to_string()),
            icon_uri: None,
        };
        assert!(matches!(
            ensure_aumid_branded(&state, "Meetily", "file:///x.ico"),
            BrandingAction::Write { .. }
        ));
    }

    #[test]
    fn display_name_differs_writes_both() {
        let state = AumidState {
            display_name: Some("OldName".to_string()),
            icon_uri: Some("file:///x.ico".to_string()),
        };
        assert!(matches!(
            ensure_aumid_branded(&state, "Meetily", "file:///x.ico"),
            BrandingAction::Write { .. }
        ));
    }

    #[test]
    fn icon_uri_differs_writes_both() {
        let state = AumidState {
            display_name: Some("Meetily".to_string()),
            icon_uri: Some("file:///old.ico".to_string()),
        };
        assert!(matches!(
            ensure_aumid_branded(&state, "Meetily", "file:///x.ico"),
            BrandingAction::Write { .. }
        ));
    }

    #[test]
    fn write_carries_exact_desired_values() {
        let state = AumidState::default();
        match ensure_aumid_branded(&state, "Meetily", "file:///path/to/icon.ico") {
            BrandingAction::Write { display_name, icon_uri } => {
                assert_eq!(display_name, "Meetily");
                assert_eq!(icon_uri, "file:///path/to/icon.ico");
            }
            BrandingAction::NoOp => panic!("expected Write"),
        }
    }
}

#[cfg(target_os = "windows")]
mod win {
    use super::{ensure_aumid_branded, AumidState, BrandingAction};
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    // CREATE_NO_WINDOW — prevents a transient reg.exe console flash.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    const DESIRED_DISPLAY_NAME: &str = "Meetily";
    const DISPLAY_NAME_VALUE: &str = "DisplayName";
    const ICON_URI_VALUE: &str = "IconUri";

    fn subkey(identifier: &str) -> String {
        format!("Software\\Classes\\AppUserModelId\\{}", identifier)
    }

    // Source-tree icon path — correct for `tauri dev`; absent in installed builds
    // (which rely on installer AUMID branding), so branding is skipped there.
    fn dev_icon_uri() -> Option<String> {
        let path = format!("{}\\icons\\icon.ico", env!("CARGO_MANIFEST_DIR"));
        if std::path::Path::new(&path).exists() {
            Some(format!("file:///{}", path.replace('\\', "/")))
        } else {
            None
        }
    }

    // `reg query "HKCU\<key>" /v <name>` prints one REG_SZ line; take the token after "REG_SZ".
    fn read_value(key: &str, value_name: &str) -> Option<String> {
        let out = Command::new("reg.exe")
            .args(["query", &format!("HKCU\\{key}"), "/v", value_name])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find(|l| l.contains("REG_SZ"))
            .and_then(|l| l.split("REG_SZ").nth(1))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn read_state(key: &str) -> AumidState {
        AumidState {
            display_name: read_value(key, DISPLAY_NAME_VALUE),
            icon_uri: read_value(key, ICON_URI_VALUE),
        }
    }

    fn write_value(key: &str, value_name: &str, val: &str) -> std::io::Result<()> {
        let status = Command::new("reg.exe")
            .args([
                "add", &format!("HKCU\\{key}"), "/v", value_name, "/t", "REG_SZ", "/d", val, "/f",
            ])
            .creation_flags(CREATE_NO_WINDOW)
            .status()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("reg add failed for {value_name}"),
            ))
        }
    }

    /// Idempotently brand the AUMID registry key at startup so dev-build OS toasts
    /// are not silently dropped. Non-fatal: any registry failure is warn-logged.
    pub fn brand_aumid_at_startup(identifier: &str) {
        let icon_uri = match dev_icon_uri() {
            Some(u) => u,
            None => {
                log::info!("AUMID branding skipped: source-tree icon not found (installed build?)");
                return;
            }
        };
        if identifier.is_empty() {
            log::warn!("AUMID branding skipped: empty app identifier");
            return;
        }
        let key = subkey(identifier);
        let current = read_state(&key);
        match ensure_aumid_branded(&current, DESIRED_DISPLAY_NAME, &icon_uri) {
            BrandingAction::NoOp => log::info!("AUMID already branded; no-op"),
            BrandingAction::Write { display_name, icon_uri } => {
                match write_value(&key, DISPLAY_NAME_VALUE, &display_name)
                    .and_then(|_| write_value(&key, ICON_URI_VALUE, &icon_uri))
                {
                    Ok(()) => log::info!("AUMID branded for dev-build toasts"),
                    Err(e) => log::warn!("AUMID branding write failed (non-fatal): {e}"),
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
pub use win::brand_aumid_at_startup;
