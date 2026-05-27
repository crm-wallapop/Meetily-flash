import { invoke } from "@tauri-apps/api/core";

export interface SpeakerInfo {
  id: string;
  name: string;
  color: string;
  created_at: string;
  updated_at: string;
}

export async function labelSpeaker(
  meetingId: string,
  clusterLabel: string,
  speakerName: string
): Promise<string> {
  return invoke<string>("label_speaker", {
    meetingId,
    clusterLabel,
    speakerName,
  });
}

export async function listSpeakers(): Promise<SpeakerInfo[]> {
  const value = await invoke<unknown>("list_speakers_cmd");
  if (Array.isArray(value)) return value as SpeakerInfo[];
  return [];
}

export async function removeSpeaker(speakerId: string): Promise<boolean> {
  return invoke<boolean>("remove_speaker_cmd", { speakerId });
}

export async function rediarizeMeeting(meetingId: string): Promise<number> {
  return invoke<number>("rediarize_meeting", { meetingId });
}

const COLOR_PALETTE = [
  "#4A90D9", "#7B68EE", "#20B2AA", "#FF6B6B", "#FFA500",
  "#9370DB", "#3CB371", "#FF69B4", "#6495ED", "#F08080",
];

export function getSpeakerColor(index: number): string {
  return COLOR_PALETTE[index % COLOR_PALETTE.length];
}
