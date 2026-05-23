use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicI32, Ordering as AtomicOrdering};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, Manager, Runtime};
use tauri_plugin_store::StoreExt;

// ── Defaults ────────────────────────────────────────────────────────────────

const DEFAULT_SCHEDULING_MODE: &str = "polite";
const DEFAULT_CPU_PAUSE_THRESHOLD_PCT: i32 = 70;
const DEFAULT_CPU_PAUSE_DURATION_SECS: i32 = 30;
const DEFAULT_RAM_PAUSE_THRESHOLD_PCT: i32 = 80;
const DEFAULT_RAM_PAUSE_DURATION_SECS: i32 = 30;

// ── Scheduling mode ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SchedulingMode {
    Aggressive,
    Polite,
    Manual,
}

impl SchedulingMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Aggressive => "aggressive",
            Self::Polite => "polite",
            Self::Manual => "manual",
        }
    }
}

impl Default for SchedulingMode {
    fn default() -> Self {
        Self::Polite
    }
}

impl std::str::FromStr for SchedulingMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "aggressive" => Ok(Self::Aggressive),
            "polite" => Ok(Self::Polite),
            "manual" => Ok(Self::Manual),
            other => Err(format!("invalid scheduling_mode: {other}")),
        }
    }
}

// ── Settings struct ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SchedulerSettings {
    #[serde(default = "default_scheduling_mode")]
    pub scheduling_mode: String,
    #[serde(default = "default_cpu_pause_threshold_pct")]
    pub cpu_pause_threshold_pct: i32,
    #[serde(default = "default_cpu_pause_duration_secs")]
    pub cpu_pause_duration_secs: i32,
    #[serde(default = "default_ram_pause_threshold_pct")]
    pub ram_pause_threshold_pct: i32,
    #[serde(default = "default_ram_pause_duration_secs")]
    pub ram_pause_duration_secs: i32,
}

fn default_scheduling_mode() -> String { DEFAULT_SCHEDULING_MODE.to_string() }
fn default_cpu_pause_threshold_pct() -> i32 { DEFAULT_CPU_PAUSE_THRESHOLD_PCT }
fn default_cpu_pause_duration_secs() -> i32 { DEFAULT_CPU_PAUSE_DURATION_SECS }
fn default_ram_pause_threshold_pct() -> i32 { DEFAULT_RAM_PAUSE_THRESHOLD_PCT }
fn default_ram_pause_duration_secs() -> i32 { DEFAULT_RAM_PAUSE_DURATION_SECS }

impl Default for SchedulerSettings {
    fn default() -> Self {
        Self {
            scheduling_mode: DEFAULT_SCHEDULING_MODE.to_string(),
            cpu_pause_threshold_pct: DEFAULT_CPU_PAUSE_THRESHOLD_PCT,
            cpu_pause_duration_secs: DEFAULT_CPU_PAUSE_DURATION_SECS,
            ram_pause_threshold_pct: DEFAULT_RAM_PAUSE_THRESHOLD_PCT,
            ram_pause_duration_secs: DEFAULT_RAM_PAUSE_DURATION_SECS,
        }
    }
}

// ── Live config (atomics for lock-free reads from the scheduler hot path) ──

pub struct SchedulerLiveConfig {
    pub mode: std::sync::Mutex<SchedulingMode>,
    pub cpu_threshold_pct: AtomicI32,
    pub cpu_duration_secs: AtomicI32,
    pub ram_threshold_pct: AtomicI32,
    pub ram_duration_secs: AtomicI32,
}

impl SchedulerLiveConfig {
    pub fn from_settings(settings: &SchedulerSettings) -> Self {
        let mode = settings
            .scheduling_mode
            .parse()
            .unwrap_or(SchedulingMode::Polite);
        Self {
            mode: std::sync::Mutex::new(mode),
            cpu_threshold_pct: AtomicI32::new(settings.cpu_pause_threshold_pct),
            cpu_duration_secs: AtomicI32::new(settings.cpu_pause_duration_secs),
            ram_threshold_pct: AtomicI32::new(settings.ram_pause_threshold_pct),
            ram_duration_secs: AtomicI32::new(settings.ram_pause_duration_secs),
        }
    }

    pub fn apply(&self, settings: &SchedulerSettings) {
        if let Ok(mode) = settings.scheduling_mode.parse::<SchedulingMode>() {
            if let Ok(mut m) = self.mode.lock() {
                *m = mode;
            }
        }
        self.cpu_threshold_pct
            .store(settings.cpu_pause_threshold_pct, AtomicOrdering::Relaxed);
        self.cpu_duration_secs
            .store(settings.cpu_pause_duration_secs, AtomicOrdering::Relaxed);
        self.ram_threshold_pct
            .store(settings.ram_pause_threshold_pct, AtomicOrdering::Relaxed);
        self.ram_duration_secs
            .store(settings.ram_pause_duration_secs, AtomicOrdering::Relaxed);
    }

    pub fn get_mode(&self) -> SchedulingMode {
        self.mode
            .lock()
            .map(|m| *m)
            .unwrap_or_else(|e| {
                log::warn!("scheduler mode mutex poisoned, falling back to Polite: {e}");
                SchedulingMode::Polite
            })
    }

    pub fn get_settings_snapshot(&self) -> SchedulerSettings {
        SchedulerSettings {
            scheduling_mode: self.get_mode().as_str().to_string(),
            cpu_pause_threshold_pct: self.cpu_threshold_pct.load(AtomicOrdering::Relaxed),
            cpu_pause_duration_secs: self.cpu_duration_secs.load(AtomicOrdering::Relaxed),
            ram_pause_threshold_pct: self.ram_threshold_pct.load(AtomicOrdering::Relaxed),
            ram_pause_duration_secs: self.ram_duration_secs.load(AtomicOrdering::Relaxed),
        }
    }
}

// ── Load / Save (Tauri plugin store) ────────────────────────────────────────

const STORE_FILE: &str = "scheduler_settings.json";
const STORE_KEY: &str = "settings";

pub async fn load_scheduler_settings<R: Runtime>(
    app: &AppHandle<R>,
) -> SchedulerSettings {
    let store = match app.store(STORE_FILE) {
        Ok(s) => s,
        Err(e) => {
            warn!("Failed to access scheduler settings store: {e}, using defaults");
            return SchedulerSettings::default();
        }
    };

    if let Some(value) = store.get(STORE_KEY) {
        match serde_json::from_value::<SchedulerSettings>(value.clone()) {
            Ok(s) => {
                info!("Loaded scheduler settings from store");
                s
            }
            Err(e) => {
                warn!("Failed to deserialize scheduler settings: {e}, using defaults");
                SchedulerSettings::default()
            }
        }
    } else {
        info!("No stored scheduler settings found, using defaults");
        SchedulerSettings::default()
    }
}

pub async fn save_scheduler_settings<R: Runtime>(
    app: &AppHandle<R>,
    settings: &SchedulerSettings,
) -> anyhow::Result<()> {
    let store = app
        .store(STORE_FILE)
        .map_err(|e| anyhow::anyhow!("Failed to access scheduler settings store: {e}"))?;

    let value = serde_json::to_value(settings)
        .map_err(|e| anyhow::anyhow!("Failed to serialize scheduler settings: {e}"))?;

    store.set(STORE_KEY, value);
    store
        .save()
        .map_err(|e| anyhow::anyhow!("Failed to save scheduler settings: {e}"))?;

    info!("Saved scheduler settings to store");
    Ok(())
}

// ── Tauri commands ──────────────────────────────────────────────────────────

#[tauri::command]
pub async fn get_scheduler_settings<R: Runtime>(
    app: AppHandle<R>,
) -> Result<SchedulerSettings, String> {
    Ok(load_scheduler_settings(&app).await)
}

#[tauri::command]
pub async fn save_scheduler_settings_cmd<R: Runtime>(
    app: AppHandle<R>,
    settings: SchedulerSettings,
) -> Result<(), String> {
    save_scheduler_settings(&app, &settings)
        .await
        .map_err(|e| format!("Failed to save scheduler settings: {e}"))?;

    // Hot-reload: update the live config so the scheduler picks up changes
    // without app restart.
    if let Some(live) = app.try_state::<Arc<SchedulerLiveConfig>>() {
        live.apply(&settings);
        info!("Scheduler live config hot-reloaded from settings change");
    }

    let _ = app.emit("scheduler-settings-changed", &settings);

    Ok(())
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_match_spec() {
        let s = SchedulerSettings::default();
        assert_eq!(s.scheduling_mode, "polite");
        assert_eq!(s.cpu_pause_threshold_pct, 70);
        assert_eq!(s.cpu_pause_duration_secs, 30);
        assert_eq!(s.ram_pause_threshold_pct, 80);
        assert_eq!(s.ram_pause_duration_secs, 30);
    }

    #[test]
    fn absent_keys_return_documented_defaults() {
        // Empty JSON object — all fields fall back to serde(default) values.
        let empty: SchedulerSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(empty.scheduling_mode, "polite");
        assert_eq!(empty.cpu_pause_threshold_pct, 70);
        assert_eq!(empty.cpu_pause_duration_secs, 30);
        assert_eq!(empty.ram_pause_threshold_pct, 80);
        assert_eq!(empty.ram_pause_duration_secs, 30);

        // Partial JSON — only scheduling_mode provided, rest use defaults.
        let partial: SchedulerSettings =
            serde_json::from_str(r#"{"scheduling_mode":"aggressive"}"#).unwrap();
        assert_eq!(partial.scheduling_mode, "aggressive");
        assert_eq!(partial.cpu_pause_threshold_pct, 70);
        assert_eq!(partial.cpu_pause_duration_secs, 30);
        assert_eq!(partial.ram_pause_threshold_pct, 80);
        assert_eq!(partial.ram_pause_duration_secs, 30);
    }

    #[test]
    fn scheduling_mode_round_trips() {
        for mode in &[SchedulingMode::Aggressive, SchedulingMode::Polite, SchedulingMode::Manual] {
            let s = mode.as_str();
            let parsed: SchedulingMode = s.parse().unwrap();
            assert_eq!(*mode, parsed);
        }
        assert!("invalid".parse::<SchedulingMode>().is_err());
    }

    #[test]
    fn live_config_applies_settings() {
        let settings = SchedulerSettings {
            scheduling_mode: "aggressive".to_string(),
            cpu_pause_threshold_pct: 40,
            cpu_pause_duration_secs: 15,
            ram_pause_threshold_pct: 50,
            ram_pause_duration_secs: 20,
        };
        let live = SchedulerLiveConfig::from_settings(&SchedulerSettings::default());
        assert_eq!(live.get_mode(), SchedulingMode::Polite);
        assert_eq!(live.cpu_threshold_pct.load(AtomicOrdering::Relaxed), 70);

        live.apply(&settings);
        assert_eq!(live.get_mode(), SchedulingMode::Aggressive);
        assert_eq!(live.cpu_threshold_pct.load(AtomicOrdering::Relaxed), 40);
        assert_eq!(live.cpu_duration_secs.load(AtomicOrdering::Relaxed), 15);
        assert_eq!(live.ram_threshold_pct.load(AtomicOrdering::Relaxed), 50);
        assert_eq!(live.ram_duration_secs.load(AtomicOrdering::Relaxed), 20);
    }

    #[test]
    fn live_config_snapshot_round_trips() {
        let settings = SchedulerSettings {
            scheduling_mode: "manual".to_string(),
            cpu_pause_threshold_pct: 55,
            cpu_pause_duration_secs: 45,
            ram_pause_threshold_pct: 65,
            ram_pause_duration_secs: 55,
        };
        let live = SchedulerLiveConfig::from_settings(&settings);
        let snapshot = live.get_settings_snapshot();
        assert_eq!(snapshot.scheduling_mode, "manual");
        assert_eq!(snapshot.cpu_pause_threshold_pct, 55);
        assert_eq!(snapshot.cpu_pause_duration_secs, 45);
        assert_eq!(snapshot.ram_pause_threshold_pct, 65);
        assert_eq!(snapshot.ram_pause_duration_secs, 55);
    }
}
