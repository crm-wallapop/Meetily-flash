pub mod google_cidrs;
pub mod titles;

#[cfg(target_os = "windows")]
pub mod browser_process;

#[cfg(target_os = "windows")]
pub mod signaling;

#[cfg(target_os = "windows")]
pub mod windows;

// Dev/test-only fake detector; absent from the binary unless `dev-detector` is on.
#[cfg(feature = "dev-detector")]
pub mod fake;
