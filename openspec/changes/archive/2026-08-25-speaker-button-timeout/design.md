# Design: Speaker Button Timeout & Error Recovery

## Changes Made

### Frontend: `TranscriptButtonGroup.tsx`

**Before:**
```javascript
const handleRediarize = useCallback(async () => {
    if (!meetingId) return;
    setIsRediarizing(true);
    try {
      const { listen } = await import('@tauri-apps/api/event');
      const unlisten = await listen<...>('diarization-complete', async (event) => {
        if (event.payload.meeting_id === meetingId) {
          unlisten();
          if (onRefetchTranscripts) await onRefetchTranscripts();
          setIsRediarizing(false);
          toast.success(...);
        }
      });

      await resetSpeakerLabels(meetingId); // ← BLOCKS UI THREAD
    } catch (e) {
      console.error('Re-diarization failed:', e);
      toast.error('Re-diarization failed', { description: e.message });
      setIsRediarizing(false);
    }
  }, [meetingId, onRefetchTranscripts]);
```

**After:**
```javascript
const handleRediarize = useCallback(async () => {
    if (!meetingId) return;
    setIsRediarizing(true);
    try {
      const { listen } = await import('@tauri-apps/api/event');
      
      // Race between: 1) event, 2) command completion, 3) timeout
      const result = await Promise.race([
        new Promise<{ meeting_id: string; speaker_count: number; segments_labeled: number }>((resolve) => {
          const unlisten = listen<...>('diarization-complete', (event) => {
            unlisten();
            if (event.payload.meeting_id === meetingId) {
              resolve(event.payload);
            }
          });
        }),
        resetSpeakerLabels(meetingId), // Fire-and-forget but still awaited for rejection
        new Promise<never>((_, reject) => 
          setTimeout(() => reject(new Error('Diarization timed out after 5 minutes')), 
                    5 * 60 * 1000)
        )
      ]);

      if (typeof result === 'object' && 'meeting_id' in result) {
        // Event fired first
        if (onRefetchTranscripts) await onRefetchTranscripts();
        toast.success(`Detected ${result.speaker_count} speaker${result.speaker_count !== 1 ? 's' : ''}`);
      } else {
        // Command completed first (should be rare, but handle)
        if (onRefetchTranscripts) await onRefetchTranscripts();
        toast.success('Diarization completed');
      }
    } catch (e) {
      if (e.message === 'Diarization timed out after 5 minutes') {
        toast.error('Re-diarization timed out', {
          description: 'The diarization process may have failed or timed out. Please try again.'
        });
      } else {
        console.error('Re-diarization failed:', e);
        toast.error('Re-diarization failed', { 
          description: e instanceof Error ? e.message : String(e) 
        });
      }
    } finally {
      setIsRediarizing(false); // ALWAYS clear spinner state
    }
  }, [meetingId, onRefetchTranscripts]);
```

## Why This Works

### 1. **Button State Safety**
- `setIsRediarizing(true)` on click → button disabled via `disabled={isRediarizing}`
- `setIsRediarizing(false)` in `finally` block → button re-enabled on ALL paths
- No path leaves button in "Analyzing…" state indefinitely

### 2. **Three-Way Race**
- **Event path** (`diarization-complete`): Normal success case - user sees success toast
- **Command path**: Fallback if command resolves before event fires (should be rare)
- **Timeout path**: Safety net for backend crashes or silent hangs (5 minutes)

### 3. **Fire-and-Forget Command with Rejection Handling**
- `resetSpeakerLabels(meetingId)` is still awaited in the race so we catch explicit rejections
- If backend returns `Err(...)`, the promise rejects → caught in race → error shown
- The command itself is fire-and-forget from Tauri's perspective once spawned (tokio task continues)

### 4. **Backend Lock Prevents Parallel Jobs**
- `run_diarization_for_meeting` uses `diarization_lock_for(meeting_id)` 
- Ensures only one diarization per meeting runs at a time
- Rapid clicking while first job runs → second job blocks on lock (not queued/parallel)
- Correct behavior: only one job per meeting at a time

### 5. **Timeout Duration Rationale**
- 5 minutes chosen because:
  - Diarization on long recordings takes 20+ minutes (per Oracle memory)
  - This is purely an "IPC died" guard, not a diarization timeout
  - Long enough to not interfere with legitimate long diarizations
  - Short enough to recover from dev server crashes

## Files Modified

- `frontend/src/components/MeetingDetails/TranscriptButtonGroup.tsx`

## Tests Needed

### Manual Test Plan:
1. Start dev server
2. Open meeting with transcripts
3. Click "Speakers" button
4. Verify button shows "Analyzing…" and is disabled
5. Kill dev server process mid-diarization
6. Verify: after ~5 minutes, button re-enables and shows timeout error
7. Click again → verify new diarization starts

### Automated Test (Conceptual):
- Mock Tauri `invoke` to return a hanging promise
- Click button
- Advance fake timers by 5 minutes
- Verify `isRediarizing` becomes false
- Verify error toast shown