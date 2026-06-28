// GUI subsystem in BOTH debug and release. A console-subsystem dev exe flashes a
// console window for the single-instance secondary that forwards each meetily://
// activation (B3). Logging still reaches the `tauri dev` terminal via inherited
// stdio handles — the subsystem attribute governs console auto-attachment, not
// whether the parent's already-open stderr handle is inherited.
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use log;
use env_logger;

fn main() {
    if std::env::var("RUST_LOG").is_err() {
        std::env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    // Async logger will be initialized lazily when first needed (after Tauri runtime starts)
    log::info!("Starting application...");
    app_lib::run();
}

#[cfg(test)]
mod tests {
    #[test]
    fn windows_subsystem_applies_in_debug_too() {
        // B3: the dev exe must be GUI-subsystem or the single-instance secondary that
        // forwards each meetily:// activation flashes a console. The fix is one cfg_attr
        // token; this test makes re-introducing the `not(debug_assertions)` guard fail
        // the build rather than silently regress.
        let src = include_str!("main.rs");
        let cfg_attr = src
            .lines()
            .find(|l| l.contains("windows_subsystem"))
            .expect("windows_subsystem cfg_attr must be present in main.rs");
        assert!(
            !cfg_attr.contains("not(debug_assertions)"),
            "windows_subsystem must apply in debug builds (B3 meetily:// console flash)"
        );
    }
}
