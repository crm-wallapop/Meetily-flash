pub mod google_cidrs;

#[cfg(target_os = "windows")]
pub mod windows;

// Dev/test-only fake detector; absent from the binary unless `dev-detector` is on.
#[cfg(feature = "dev-detector")]
pub mod fake;
