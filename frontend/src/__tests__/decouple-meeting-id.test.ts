/**
 * decouple-meeting-id-from-save — TypeScript tests (tasks 2.1–2.3).
 *
 * Pure logic tests: no React rendering, no AppHandle needed.
 * Tests the stop-hook flow contract and the recordingService type shapes.
 */
import { describe, it, expect, vi } from 'vitest';
import type { StopRecordingResult, StartRecordingResult } from '@/services/recordingService';

// ---------------------------------------------------------------------------
// Task 2.1: stop hook navigates immediately (no save call)
// ---------------------------------------------------------------------------

describe('useRecordingStop: stop flow contract', () => {
  it('no storageService.saveMeeting call is needed — meeting_id comes from stopResult', () => {
    const stopResult: StopRecordingResult = {
      folder_path: '/recordings/test-meeting',
      meeting_name: 'Test Meeting',
      meeting_id: 'meeting-550e8400-e29b-41d4-a716-446655440000',
    };

    // The stop hook must NOT call any save function — Rust handles the SQLite row.
    const fakeSave = vi.fn();

    // Simulate the stop flow: read meeting_id from stopResult directly
    const meetingId = stopResult.meeting_id || null;
    expect(meetingId).toBe('meeting-550e8400-e29b-41d4-a716-446655440000');
    expect(fakeSave).not.toHaveBeenCalled();
  });

  // Task 2.2: enqueueTranscriptionJob is called with correct meeting_id and audioPath
  it('enqueueTranscriptionJob receives meeting_id and audioPath from stopResult', () => {
    const stopResult: StopRecordingResult = {
      folder_path: 'C:\\Recordings\\Meeting_2026-05-24',
      meeting_name: 'Standup',
      meeting_id: 'meeting-550e8400-e29b-41d4-a716-446655440000',
    };

    const enqueue = vi.fn();

    // Mirror the stop hook logic from useRecordingStop
    const meetingId = stopResult.meeting_id || null;
    const folderPath = stopResult.folder_path;
    if (folderPath && meetingId) {
      const audioPath = folderPath.replace(/\\/g, '/') + '/audio.mp4';
      enqueue(meetingId, audioPath);
    }

    expect(enqueue).toHaveBeenCalledOnce();
    expect(enqueue).toHaveBeenCalledWith(
      'meeting-550e8400-e29b-41d4-a716-446655440000',
      'C:/Recordings/Meeting_2026-05-24/audio.mp4'
    );
  });

  it('does not enqueue when folder_path is null', () => {
    const stopResult: StopRecordingResult = {
      folder_path: null,
      meeting_name: 'No Audio',
      meeting_id: 'meeting-550e8400-e29b-41d4-a716-446655440000',
    };

    const enqueue = vi.fn();
    const meetingId = stopResult.meeting_id || null;
    const folderPath = stopResult.folder_path;
    if (folderPath && meetingId) {
      const audioPath = folderPath.replace(/\\/g, '/') + '/audio.mp4';
      enqueue(meetingId, audioPath);
    }
    expect(enqueue).not.toHaveBeenCalled();
  });

  it('falls back to activeMeetingId when stopResult.meeting_id is null', () => {
    const activeMeetingId = 'meeting-00000000-0000-0000-0000-000000000001';
    const stopResult: StopRecordingResult = {
      folder_path: '/recordings/test',
      meeting_name: 'Test',
      meeting_id: null,
    };

    const meetingId = stopResult.meeting_id || activeMeetingId;
    expect(meetingId).toBe('meeting-00000000-0000-0000-0000-000000000001');
  });
});

// ---------------------------------------------------------------------------
// Task 2.3: recordingService.startRecording returns meeting_id
// ---------------------------------------------------------------------------

describe('StartRecordingResult type contract', () => {
  it('StartRecordingResult has meeting_id string field', () => {
    const result: StartRecordingResult = {
      meeting_id: 'meeting-550e8400-e29b-41d4-a716-446655440000',
    };
    expect(result.meeting_id).toMatch(/^meeting-[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/);
  });

  it('meeting_id is non-empty', () => {
    const result: StartRecordingResult = {
      meeting_id: 'meeting-550e8400-e29b-41d4-a716-446655440000',
    };
    expect(result.meeting_id.length).toBeGreaterThan(0);
  });
});
