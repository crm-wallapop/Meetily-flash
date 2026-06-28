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
