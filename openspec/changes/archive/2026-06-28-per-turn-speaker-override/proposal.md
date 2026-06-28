# Proposal: per-turn-speaker-override

## User-facing summary (plain English)

You can now fix the speaker label on a single paragraph without renaming every paragraph from that speaker. Next to each paragraph, the speaker badge's name field gains a checkbox — "Also rename all 'Speaker 2' segments" — checked by default (today's behavior: rename the whole group). Uncheck it and only the one paragraph you're editing changes. This is the surgical-correction tool for meetings where diarization got a few turns wrong but most of the grouping is right.

## Why

On the broken 2026-06-22 meeting, diarization mis-attributed whole regions. Even after a future temporal-coherence fix lands, there will always be individual segments the user wants to correct **without relabeling every segment in the cluster**. Today the only correction path is cluster-wide: clicking a badge and typing a name runs `label_speaker`, which relabels every transcript row sharing that cluster label in the meeting. There is no way to say "just this one turn is Carlos."

The capability already exists end-to-end below the UI:

- Rust command `set_segment_speaker(transcript_id, speaker_label)`
  (`audio/speaker/commands.rs:262`)
- Repository `update_transcript_speaker_manual`
  (`database/repositories/speaker.rs:264`) — single-row UPDATE, sets
  `speaker_source = 'manual'`, preserves `previous_label` (set-once via `CASE`)
- Frontend adapter `setSegmentSpeaker(transcriptId, speakerLabel)`
  (`services/speakerService.ts:45`)
- Manual-source preservation against automatic overwrites: the auto-label
  write path (`update_transcript_speaker`, `speaker.rs:244`) guards
  `WHERE speaker_source IS NULL OR speaker_source != 'manual'`, so an
  automatic label never clobbers a manual one within a diarization run.

The only missing piece is **UI wiring**: `useSpeakerRename.ts` calls
`labelSpeaker` exclusively; neither `TranscriptView.tsx:326` nor
`VirtualizedTranscriptView.tsx:364/429` ever invokes `setSegmentSpeaker`. The
capability ships, the surface to reach it does not.

### Correction to an earlier claim (re-diarization does NOT preserve per-turn overrides)

An earlier draft of this proposal claimed per-turn overrides "survive
re-diarization." That is **false for the UI-reachable button.** The "Speakers"
re-diarize button (`TranscriptButtonGroup.tsx:54`) calls `reset_speaker_labels`
(`speakerService.ts:37`) → `reset_speaker_labels` (`commands.rs:215`) →
`clear_all_speaker_labels` (`speaker.rs:313`), which runs
`UPDATE transcripts SET speaker_label = NULL, speaker_source = NULL,
previous_label = NULL WHERE meeting_id = ?` — no source filter, so it clears
ALL rows including manual. The canonical spec's "Re-diarization cleans up stale
state" requirement mandates exactly this (clear both auto AND manual). So a
per-turn override IS destroyed by the Speakers button, exactly like a cluster
manual label. The auto-write `!= 'manual'` guard only matters for partial
re-runs that skip the full reset — not for the user-facing button. This change
does not alter that behavior; it inherits it.

## What Changes

- Add a **scope toggle** to `SpeakerLabelInput`: a checkbox
  "Also rename all '<current cluster label>' segments" — **checked by default**
  (preserves today's cluster-rename behavior with zero regression).
  - Checked → existing `labelSpeaker(meetingId, clusterLabel, name)` path.
  - Unchecked → `setSegmentSpeaker(transcriptId, name)` (single-row override).
- Thread `transcriptId` and `scope` through `onSubmit` →
  `useSpeakerRename.handleSpeakerSubmit`, which branches on scope. The
  signature ripples through the `TranscriptSegment` component's `onSpeakerSubmit`
  prop (`VirtualizedTranscriptView.tsx`) and the `TranscriptView.tsx:326`
  closure — all call sites gain `(name, scope)`.
- **Bring the TranscriptView color fix into scope** (see design D6):
  `TranscriptView.tsx:333` derives `colorIndex` via
  `parseInt(speaker.replace("Speaker ",""))` which is `NaN` for named labels,
  collapsing every named speaker to color index 0. The per-turn path makes
  this visible (adjacent "Speaker 2" correct-color + "Carlos" wrong-color
  segments). Adopt the `speakerIndexMap` (first-appearance order) approach
  already used by `VirtualizedTranscriptView.tsx:273` so named labels render
  distinct, correct colors in both views.
- Wire the `set_segment_speaker` smoke mock in `_meeting-details.ts` to record
  into `__smokeSpeakerCalls` (it is currently a no-op returning `true`), so
  smoke tests can assert dispatch.
- No new Tauri command, port, repository method, or DB column.

## Capabilities

- `speaker-diarization` — extends inline labeling from cluster-only to
  cluster-or-single-segment, reusing the existing `set_segment_speaker` path.

## Impact

- **Files touched:**
  - `frontend/src/components/SpeakerBadge.tsx` — `SpeakerLabelInput` gains the
    scope checkbox; `onSubmit` signature gains `scope`.
  - `frontend/src/hooks/useSpeakerRename.ts` — import `setSegmentSpeaker`;
    `handleSpeakerSubmit` branches on scope.
  - `frontend/src/components/TranscriptView.tsx` — thread `(name, scope)`,
    pass `transcript.id`; **fix `colorIndex` derivation** (speakerIndexMap).
  - `frontend/src/components/VirtualizedTranscriptView.tsx` — thread
    `(name, scope)` + `segment.id` through the `TranscriptSegment` call sites.
  - `frontend/src/components/TranscriptSegment` (if separate) —
    `onSpeakerSubmit` prop type gains `scope`.
  - `frontend/e2e/smoke/_meeting-details.ts` — record `set_segment_speaker`
    dispatch in the smoke mock.
- **Behavior change:** default badge-rename flow is unchanged (checkbox
  defaults to cluster). The new per-turn path is opt-in. Per-turn overrides
  are cleared by the Speakers re-diarize button (inherited behavior, matching
  the canonical spec).
- **Risk:** low for the data path (backend is shipped). Main risks are UX
  (discoverability — mitigated by default-checked) and the TranscriptView
  color fix (small, well-understood pattern borrowed from the sibling view).
- **Sequencing:** touches the same files as `speaker-rename-cancel`. Apply
  `speaker-rename-cancel` first, then this one. Independent of
  `diarization-temporal-coherence` (backend). The three compose.
