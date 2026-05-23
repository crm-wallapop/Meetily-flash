import { invoke } from "@tauri-apps/api/core";

export type SchedulingMode = "aggressive" | "polite" | "manual";

export interface SchedulerSettings {
  scheduling_mode: SchedulingMode;
  cpu_pause_threshold_pct: number;
  cpu_pause_duration_secs: number;
  ram_pause_threshold_pct: number;
  ram_pause_duration_secs: number;
}

const DEFAULTS: SchedulerSettings = {
  scheduling_mode: "polite",
  cpu_pause_threshold_pct: 70,
  cpu_pause_duration_secs: 30,
  ram_pause_threshold_pct: 80,
  ram_pause_duration_secs: 30,
};

export function defaultSchedulerSettings(): SchedulerSettings {
  return { ...DEFAULTS };
}

export async function getSchedulerSettings(): Promise<SchedulerSettings> {
  const settings = await invoke<SchedulerSettings>("get_scheduler_settings");
  return { ...DEFAULTS, ...settings };
}

export async function saveSchedulerSettings(
  settings: SchedulerSettings
): Promise<void> {
  await invoke("save_scheduler_settings_cmd", { settings });
}

export async function runTranscriptionJobNow(
  meetingId: string
): Promise<boolean> {
  return invoke<boolean>("run_transcription_job_now", { meetingId });
}

export function validateSchedulerSettings(
  settings: SchedulerSettings
): Record<string, string> {
  const errors: Record<string, string> = {};
  if (settings.cpu_pause_threshold_pct < 1 || settings.cpu_pause_threshold_pct > 100) {
    errors.cpu_pause_threshold_pct = "Must be 1–100";
  }
  if (settings.cpu_pause_duration_secs < 5 || settings.cpu_pause_duration_secs > 600) {
    errors.cpu_pause_duration_secs = "Must be 5–600";
  }
  if (settings.ram_pause_threshold_pct < 1 || settings.ram_pause_threshold_pct > 100) {
    errors.ram_pause_threshold_pct = "Must be 1–100";
  }
  if (settings.ram_pause_duration_secs < 5 || settings.ram_pause_duration_secs > 600) {
    errors.ram_pause_duration_secs = "Must be 5–600";
  }
  return errors;
}
