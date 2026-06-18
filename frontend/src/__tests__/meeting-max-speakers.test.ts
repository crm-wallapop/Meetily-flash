import { describe, it, expect, vi, beforeEach } from 'vitest';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
import { invoke } from '@tauri-apps/api/core';
import {
  getMeetingMaxSpeakers,
  setMeetingMaxSpeakers,
} from '@/services/speakerService';
import { isValidMeetingCap } from '@/components/MeetingDetails/MeetingMaxSpeakersControl';

const mockInvoke = invoke as unknown as ReturnType<typeof vi.fn>;

describe('meeting max speakers adapters', () => {
  beforeEach(() => mockInvoke.mockReset());

  it('getMeetingMaxSpeakers invokes get_meeting_max_speakers with the meeting id', async () => {
    mockInvoke.mockResolvedValue({
      override: 3,
      effective: 3,
      global_default: 10,
    });
    const r = await getMeetingMaxSpeakers('m1');
    expect(mockInvoke).toHaveBeenCalledWith('get_meeting_max_speakers', {
      meetingId: 'm1',
    });
    expect(r.override).toBe(3);
    expect(r.effective).toBe(3);
    expect(r.global_default).toBe(10);
  });

  it('setMeetingMaxSpeakers invokes set_meeting_max_speakers with meetingId + cap', async () => {
    mockInvoke.mockResolvedValue(undefined);
    await setMeetingMaxSpeakers('m1', 3);
    expect(mockInvoke).toHaveBeenCalledWith('set_meeting_max_speakers', {
      meetingId: 'm1',
      cap: 3,
    });
  });

  it('setMeetingMaxSpeakers passes null to clear the override', async () => {
    mockInvoke.mockResolvedValue(undefined);
    await setMeetingMaxSpeakers('m1', null);
    expect(mockInvoke).toHaveBeenCalledWith('set_meeting_max_speakers', {
      meetingId: 'm1',
      cap: null,
    });
  });

  it('does NOT invoke any other command (no auto-rediarize wiring in the adapter)', async () => {
    mockInvoke.mockResolvedValue(undefined);
    await setMeetingMaxSpeakers('m1', 4);
    expect(mockInvoke).toHaveBeenCalledTimes(1);
    expect(mockInvoke.mock.calls[0][0]).toBe('set_meeting_max_speakers');
  });
});

describe('isValidMeetingCap', () => {
  it('rejects values below 2', () => {
    expect(isValidMeetingCap(1)).toBe(false);
    expect(isValidMeetingCap(0)).toBe(false);
    expect(isValidMeetingCap(-3)).toBe(false);
  });

  it('rejects values above 20', () => {
    expect(isValidMeetingCap(21)).toBe(false);
    expect(isValidMeetingCap(100)).toBe(false);
  });

  it('accepts the inclusive bounds 2 and 20', () => {
    expect(isValidMeetingCap(2)).toBe(true);
    expect(isValidMeetingCap(20)).toBe(true);
  });

  it('accepts mid-range integers', () => {
    expect(isValidMeetingCap(3)).toBe(true);
    expect(isValidMeetingCap(10)).toBe(true);
  });

  it('rejects non-integers', () => {
    expect(isValidMeetingCap(2.5)).toBe(false);
    expect(isValidMeetingCap(NaN)).toBe(false);
  });
});
