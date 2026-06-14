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

export async function resetSpeakerLabels(meetingId: string): Promise<number> {
  return invoke<number>("reset_speaker_labels", { meetingId });
}

export async function revertSpeakerLabel(meetingId: string, speakerLabel: string): Promise<number> {
  return invoke<number>("revert_speaker_label", { meetingId, speakerLabel });
}

export async function setSegmentSpeaker(transcriptId: string, speakerLabel: string): Promise<boolean> {
  return invoke<boolean>("set_segment_speaker", { transcriptId, speakerLabel });
}

export async function getSpeakerMergeThreshold(): Promise<number> {
  return invoke<number>("get_speaker_merge_threshold");
}

export async function setSpeakerMergeThreshold(threshold: number): Promise<void> {
  return invoke<void>("set_speaker_merge_threshold", { threshold });
}

export type SpeakerEmbeddingModel = "3dspeaker" | "wespeaker" | "nemo_titanet" | "eres2net";

export const SPEAKER_EMBEDDING_MODELS: { value: SpeakerEmbeddingModel; label: string }[] = [
  { value: "3dspeaker", label: "3DSpeaker CAM++ (zh-cn, ~38 MB)" },
  { value: "nemo_titanet", label: "NeMo Titanet Small (EN VoxCeleb, ~38 MB)" },
  { value: "eres2net", label: "3DSpeaker ERes2Net (EN VoxCeleb, ~25 MB)" },
  { value: "wespeaker", label: "WeSpeaker ResNet34 (EN VoxCeleb, ~25 MB)" },
];

export async function getSpeakerEmbeddingModel(): Promise<SpeakerEmbeddingModel> {
  return invoke<SpeakerEmbeddingModel>("get_speaker_embedding_model");
}

export async function setSpeakerEmbeddingModel(model: SpeakerEmbeddingModel): Promise<void> {
  return invoke<void>("set_speaker_embedding_model", { model });
}

export async function checkEmbeddingModelAvailable(model: SpeakerEmbeddingModel): Promise<boolean> {
  return invoke<boolean>("check_embedding_model_available", { model });
}

export async function downloadEmbeddingModel(model: SpeakerEmbeddingModel): Promise<void> {
  return invoke<void>("download_embedding_model", { model });
}

export async function getMaxSpeakers(): Promise<number> {
  return invoke<number>("get_max_speakers");
}

export async function setMaxSpeakers(cap: number): Promise<void> {
  return invoke<void>("set_max_speakers", { cap });
}

export async function getDiarizationEnabled(): Promise<boolean> {
  return invoke<boolean>("get_diarization_enabled");
}

export async function setDiarizationEnabled(enabled: boolean): Promise<void> {
  return invoke<void>("set_diarization_enabled", { enabled });
}

export function getSpeakerColor(index: number): string {
  const hue = (index * 137.508) % 360;
  return `hsl(${hue}, 65%, 55%)`;
}
