# Proposal: Speaker Button Timeout & Error Recovery

## Summary

Fixes the "Speakers" button freezing in the "Analyzing…" state indefinitely when the backend dies mid-diarization (dev server crash, command timeout), and adds explicit timeout/error recovery so users can retry. The button currently awaits the `resetSpeakerLabels` command directly, which blocks the UI thread until the command resolves or rejects — if the backend hangs silently, the spinner is permanent.

## Problem Statement

**Symptoms:**
1. User clicks "Speakers" button → spinner shows "Analyzing…"
2. Backend dies mid-diarization (dev server crash, IPC timeout)
3. `resetSpeakerLabels` command promise hangs forever
4. Button stays disabled → user cannot retry
5. Tags were deleted but no new labels generated

**Root causes:**
- `resetSpeakerLabels` in `TranscriptButtonGroup.tsx` awaits the command directly — no timeout guard
- Diarization can take 20+ minutes on long recordings; a 60-second Tauri command timeout can kill the IPC while the background `tokio::task` continues
- The `diarization-complete` event is the only reliable completion signal, but it fires from inside `tokio::spawn` after the task completes — not from the command's return path

**Evidence:** Backend logs showed zero `reset_speaker_labels` entries when the user clicked the button — meaning the command was invoked but the Promise hung without resolving or rejecting, leaving the UI frozen.

## Current Behavior

```typescript
const handleRediarize = useCallback(async () => {
  setIsRediarizing(true);   // Button disabled
  try {
    await listen('diarization-complete', callback); // Register event
    await resetSpeakerLabels(meetingId);            // AWAIT command
  } catch (e) {
    setIsRediarizing(false); // Only clears if listen() throws
  }
});
```

- If `listen()` throws → error shown, button re-enables ✓
- If `resetSpeakerLabels` hangs → button permanently stuck ✗
- Backend dies mid-diarization → no error, no recovery ✗

## Proposed Behavior

Three completion paths race each other:

1. **`diarization-complete` event fires** (primary success path) → success toast + refetch + button re-enables
2. **Command completes normally** (fallback, rare) → success toast + button re-enables  
3. **5-minute safety timeout** → error toast + button re-enables + user can retry

```typescript
const result = await Promise.race([
  diarizationCompletePromise,   // Event
  resetSpeakerLabels(meetingId), // Command
  new Promise(_, (_, reject) => 
    setTimeout(() => reject(new Error('timeout')), 5 * 60 * 1000)
  )
]);
```

**Backend constraint:** The per-meeting `diarization_lock` in `commands.rs` prevents concurrent diarization jobs for the same meeting — even if the button is re-enabled and clicked again, the second job blocks until the first completes. This is the correct behavior.

## Acceptance Criteria

- [ ] Button goes disabled on click and stays disabled while diarization runs
- [ ] Button re-enables within 5 minutes max (timeout safety net)
- [ ] Button re-enables immediately on `diarization-complete` event
- [ ] Error toast shown if timeout fires
- [ ] User can click again to retry after timeout or error
- [ ] No parallel diarization jobs for the same meeting (enforced by backend lock)
- [ ] Tags are always cleared on click (not dependent on success path)

## Out of Scope

- Backend cancellation of in-progress diarization
- Progress indicator during diarization
- Backend timeout / command timeout configuration
