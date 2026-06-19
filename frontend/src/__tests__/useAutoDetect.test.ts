/**
 * Adversarial tests for the pure decision helpers extracted from useAutoDetect.
 *
 * These encode the auto-detect smoke contract — the parts of the manual smoke
 * (tasks 5.2 / 6.1 / 6.4) that are frontend reactions to the meeting-detected /
 * meeting-ended events: when to auto-start, when to show which banner, and the
 * guards that prevent the subtle regressions (double-start, stale-stop-prompt,
 * auto-stopping a manual recording). They test the REAL exported helpers the
 * hook calls, so a regression in the hook's decision logic fails these tests.
 *
 * NOT covered here (stays manual / live-device): real audio capture and the
 * audio.mp4 finalize timing — that needs a running dev-detector build and a
 * real audio device, which no browser-level test can exercise.
 */
import { describe, it, expect } from 'vitest';
import {
  shouldStartOnDetected,
  isStopPromptActiveForRedetect,
  shouldShowStopPrompt,
  detectPromptBanner,
  stopPromptBanner,
  shouldPushTitleUpdate,
} from '@/hooks/useAutoDetect';
import type { AutoDetectBannerState } from '@/hooks/useAutoDetect';

const hiddenIdle: AutoDetectBannerState = {
  visible: false,
  mode: 'detect-prompt',
  initialTitle: '',
  candidateTitles: [],
};

// ── shouldStartOnDetected ───────────────────────────────────────────────

describe('shouldStartOnDetected', () => {
  it('starts when auto-detect is on and nothing is recording', () => {
    expect(shouldStartOnDetected({ autoDetectMeetings: true, isRecording: false })).toBe(true);
  });

  it('does NOT start when already recording (no double-start, D17)', () => {
    expect(shouldStartOnDetected({ autoDetectMeetings: true, isRecording: true })).toBe(false);
  });

  it('does NOT start when auto-detect is disabled, even if idle', () => {
    expect(shouldStartOnDetected({ autoDetectMeetings: false, isRecording: false })).toBe(false);
  });

  it('does NOT start when both disabled and recording', () => {
    expect(shouldStartOnDetected({ autoDetectMeetings: false, isRecording: true })).toBe(false);
  });
});

// ── isStopPromptActiveForRedetect (D17 re-engage dismiss) ───────────────

describe('isStopPromptActiveForRedetect', () => {
  it('is active when a stop-prompt is visible', () => {
    expect(isStopPromptActiveForRedetect({ ...hiddenIdle, visible: true, mode: 'stop-prompt' })).toBe(true);
  });

  it('is not active when the stop-prompt has been dismissed (not visible)', () => {
    expect(isStopPromptActiveForRedetect({ ...hiddenIdle, visible: false, mode: 'stop-prompt' })).toBe(false);
  });

  it('is not active when the visible banner is a detect-prompt, not a stop-prompt', () => {
    expect(isStopPromptActiveForRedetect({ ...hiddenIdle, visible: true, mode: 'detect-prompt' })).toBe(false);
  });
});

// ── shouldShowStopPrompt (the multi-guard — Task 7.5) ───────────────────

describe('shouldShowStopPrompt', () => {
  const ok = {
    autoDetectMeetings: true,
    isRecording: true,
    isDetectorStarted: true,
    isUserManaged: false,
  };

  it('shows the stop-prompt when all conditions hold', () => {
    expect(shouldShowStopPrompt(ok)).toBe(true);
  });

  it('does NOT prompt when auto-detect is off', () => {
    expect(shouldShowStopPrompt({ ...ok, autoDetectMeetings: false })).toBe(false);
  });

  it('does NOT prompt when not recording (stale meeting-ended after stop)', () => {
    // The regression the render-time ref sync guards against: an event arriving
    // after recording already ended must not re-show a prompt.
    expect(shouldShowStopPrompt({ ...ok, isRecording: false })).toBe(false);
  });

  it('does NOT prompt for a manually-started recording (only detector-started)', () => {
    expect(shouldShowStopPrompt({ ...ok, isDetectorStarted: false })).toBe(false);
  });

  it('does NOT prompt after the user chose "Keep Recording" (user-managed)', () => {
    expect(shouldShowStopPrompt({ ...ok, isUserManaged: true })).toBe(false);
  });

  it('does NOT prompt when every guard is broken', () => {
    expect(
      shouldShowStopPrompt({
        autoDetectMeetings: false,
        isRecording: false,
        isDetectorStarted: false,
        isUserManaged: true,
      }),
    ).toBe(false);
  });
});

// ── banner factories ───────────────────────────────────────────────────

describe('detectPromptBanner', () => {
  it('builds a visible detect-prompt from a detected payload', () => {
    const b = detectPromptBanner({ default_title: 'Weekly Sync', candidate_titles: ['Weekly Sync', 'Standup'] });
    expect(b).toEqual({
      visible: true,
      mode: 'detect-prompt',
      initialTitle: 'Weekly Sync',
      candidateTitles: ['Weekly Sync', 'Standup'],
    });
  });

  it('tolerates an empty title (malformed / untrusted payload at the boundary)', () => {
    const b = detectPromptBanner({ default_title: '', candidate_titles: [] });
    expect(b.visible).toBe(true);
    expect(b.mode).toBe('detect-prompt');
    expect(b.initialTitle).toBe('');
    expect(b.candidateTitles).toEqual([]);
  });

  it('does not mutate the input payload', () => {
    const payload = { default_title: 'X', candidate_titles: ['X'] };
    detectPromptBanner(payload);
    expect(payload).toEqual({ default_title: 'X', candidate_titles: ['X'] });
  });
});

describe('stopPromptBanner', () => {
  it('builds a visible stop-prompt with cleared title/candidates', () => {
    expect(stopPromptBanner()).toEqual({
      visible: true,
      mode: 'stop-prompt',
      initialTitle: '',
      candidateTitles: [],
    });
  });

  it('returns a fresh object each call (no shared mutable state)', () => {
    expect(stopPromptBanner()).not.toBe(stopPromptBanner());
  });
});

// ── shouldPushTitleUpdate ──────────────────────────────────────────────

describe('shouldPushTitleUpdate', () => {
  it('pushes when the confirmed title differs from the initial', () => {
    expect(shouldPushTitleUpdate('Edited Title', 'Initial')).toBe(true);
  });

  it('does NOT push when the title is unchanged', () => {
    expect(shouldPushTitleUpdate('Same', 'Same')).toBe(false);
  });

  it('does NOT push an empty title even if it differs from the initial', () => {
    expect(shouldPushTitleUpdate('', 'Initial')).toBe(false);
  });
});
