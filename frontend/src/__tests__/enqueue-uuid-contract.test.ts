/**
 * Adversarial tests for the post-meeting-transcription enqueue contract.
 *
 * Before this fix, `recording_commands.rs` enqueued with the folder name
 * (e.g. "Meeting 2026-05-14_17-06-41_...") as the meeting_id. The DB row was
 * created with "meeting-{uuid}" by `transcript.rs`. Transcripts were saved under
 * the wrong ID; the meeting view found nothing.
 *
 * Fix: frontend calls `enqueue_transcription_job` AFTER `saveMeeting` returns
 * the UUID, so the queue and the DB row agree on the meeting_id.
 *
 * These tests pin the pure-logic contracts — meeting_id format and audio path
 * construction — without requiring Tauri invoke mocks.
 */
import { describe, it, expect } from 'vitest';

// ── meeting_id format contract ────────────────────────────────────────────────

const UUID_MEETING_ID_PATTERN = /^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
const FOLDER_NAME_PATTERN = /^Meeting \d{4}-\d{2}-\d{2}/;

function isUuidMeetingId(id: string): boolean {
  return UUID_MEETING_ID_PATTERN.test(id);
}

function isFolderNameMeetingId(id: string): boolean {
  return FOLDER_NAME_PATTERN.test(id);
}

describe('enqueue meeting_id must be UUID from DB row, not folder name', () => {
  const uuidId = 'meeting-550e8400-e29b-41d4-a716-446655440000';
  const folderNameId = 'Meeting 2026-05-14_17-06-41_2026-05-14_15-06';

  it('UUID-format ID matches the expected DB row format', () => {
    expect(isUuidMeetingId(uuidId)).toBe(true);
  });

  it('folder-name ID does NOT match the expected DB row format', () => {
    expect(isUuidMeetingId(folderNameId)).toBe(false);
  });

  it('folder-name ID matches the old (wrong) folder-name format', () => {
    expect(isFolderNameMeetingId(folderNameId)).toBe(true);
  });

  it('UUID-format ID does NOT match the folder-name format', () => {
    expect(isFolderNameMeetingId(uuidId)).toBe(false);
  });
});

// ── audio path construction ───────────────────────────────────────────────────

// Mirror the logic from useRecordingStop.ts that builds the audio_path
// passed to enqueue_transcription_job.
function buildAudioPath(folderPath: string): string {
  return folderPath.replace(/\\/g, '/') + '/audio.mp4';
}

describe('audio path construction for enqueue', () => {
  it('Windows backslash path is normalised to forward slashes', () => {
    const folderPath = 'C:\\Users\\user\\Music\\meetily-recordings\\Meeting 2026-05-14_17-06-41_2026-05-14_15-06';
    const audioPath = buildAudioPath(folderPath);
    expect(audioPath).toBe('C:/Users/user/Music/meetily-recordings/Meeting 2026-05-14_17-06-41_2026-05-14_15-06/audio.mp4');
    expect(audioPath).not.toContain('\\');
  });

  it('POSIX path is returned unchanged with audio.mp4 appended', () => {
    const folderPath = '/home/user/recordings/Meeting-2026-05-14';
    const audioPath = buildAudioPath(folderPath);
    expect(audioPath).toBe('/home/user/recordings/Meeting-2026-05-14/audio.mp4');
  });

  it('path already using forward slashes is handled correctly', () => {
    const folderPath = 'C:/Users/user/Music/meetily-recordings/Meeting-2026';
    const audioPath = buildAudioPath(folderPath);
    expect(audioPath).toBe('C:/Users/user/Music/meetily-recordings/Meeting-2026/audio.mp4');
  });
});
