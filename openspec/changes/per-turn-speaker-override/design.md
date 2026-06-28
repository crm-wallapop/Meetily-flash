## Context

Speaker-correction data flow today (cluster-only):

```
SpeakerBadge click
   │
   ▼ setEditingSegmentId(transcript.id)
SpeakerLabelInput
   │ onSubmit(name)              ← no scope concept
   ▼
useSpeakerRename.handleSpeakerSubmit(clusterLabel, name)
   │
   ▼  ALWAYS
labelSpeaker(meetingId, clusterLabel, name)   →  label_speaker (Tauri)
   │
   ▼  repository: update_meeting_speakers  (meeting-wide UPDATE WHERE speaker_label = ?)
Every row in the meeting with that cluster label is relabeled.
```

The per-turn path exists in parallel but is unreachable:

```
setSegmentSpeaker(transcriptId, name)  →  set_segment_speaker (Tauri, commands.rs:262)
   │
   ▼  repository: update_transcript_speaker_manual (speaker.rs:264)
Single-row UPDATE:  SET speaker_label=?, speaker_source='manual',
                     previous_label = CASE WHEN previous_label IS NULL
                                            THEN speaker_label  -- old value
                                            ELSE previous_label END
                     WHERE id = ?
```

Repository guarantees that DO hold (verified, no change needed):

| Guarantee | Where | Effect |
|---|---|---|
| `previous_label` set-once | `speaker.rs:271` CASE | A per-turn override on a previously-labeled row is revertible via the existing cluster revert (restores the row's own `previous_label`) |
| `source='manual'` | `speaker.rs:270` | Marks the row manual so the auto-label write path won't clobber it within a run |
| Auto-write won't clobber manual | `speaker.rs:244` `WHERE ... speaker_source != 'manual'` | A re-diarized auto label never overwrites a per-turn manual override *within a run that skips the full reset* |
| Hostile input defense | parameterized `?` binding in `speaker.rs` + `sanitize_speaker_name` (`commands.rs:268`) | SQL injection prevented by `sqlx` parameter binding (NOT by `sanitize_speaker_name`, which passes hostile strings through — see D7) |

Repository behavior that does NOT preserve overrides (correcting an earlier draft):

| Operation | What happens to a per-turn override |
|---|---|
| **"Speakers" re-diarize button** (`reset_speaker_labels` → `clear_all_speaker_labels`, `speaker.rs:313`, no source filter) | **Cleared.** ALL rows nulled, manual included. Required by the canonical "Re-diarization cleans up stale state" spec. This change inherits the behavior; it does not alter it. |
| Re-diarization auto-label step (`clear_auto_speaker_labels`, `speaker.rs:298`) | Only clears `source='auto'`. But this runs AFTER `clear_all_speaker_labels` already nuked everything, so it is a no-op on the UI path. |

**Net:** a per-turn override survives only programmatic partial re-runs, not the
user-facing Speakers button. This change does not overclaim survival.

## Goals

- User can correct a single transcript segment's speaker without relabeling the
  whole cluster.
- Default behavior (cluster rename) is preserved — zero regression.
- Per-turn overrides are revertible (via the existing cluster revert, for
  previously-labeled rows).

## Non-Goals

- Changing `set_segment_speaker`, `update_transcript_speaker_manual`, or any
  repository/command code (the auto-write guard and reset behavior are
  inherited as-is). No new commands.
- Making the Speakers re-diarize button preserve manual labels — that would
  contradict the canonical spec and is a separate decision.
- Bulk multi-select override. One segment at a time.
- The temporal-coherence clustering fix — separate change.

## Design

### D1 — UX: scope toggle inside the existing input (recommended)

Three candidate surfaces for reaching per-turn override:

| Surface | Pros | Cons |
|---|---|---|
| **Scope checkbox inside `SpeakerLabelInput`** (recommended) | One control, one place; default-checked = today's behavior; the user decides scope at the moment of typing; no new DOM region | Slightly enlarges the input popover |
| Split badge (caret → dropdown: "all" vs "this turn") | Discoverable two-mode affordance | Two open states; more components; conflicts with cancel-on-blur work |
| Right-click / context menu | Zero new chrome | Undiscoverable; doesn't exist elsewhere |

**Recommendation: the checkbox.** Reuses the existing input, defaults to
non-regressive, smallest delta. The label interpolates the current cluster
label: "☑ Also rename all 'Speaker 2' segments" — the consequence is visible
before submit.

The toggle state is local to the `SpeakerLabelInput` instance (resets each
open). No global setting, no persistence (YAGNI).

### D2 — Wiring through the hook (with TranscriptSegment ripple)

`SpeakerLabelInput.onSubmit` signature changes from `(name: string)` to
`(name: string, scope: 'cluster' | 'segment')`.

`useSpeakerRename.handleSpeakerSubmit` signature changes from
`(clusterLabel, name)` to `(transcriptId, clusterLabel, name, scope)`:

```
scope === 'cluster'  → labelSpeaker(meetingId, clusterLabel, name)     // unchanged
scope === 'segment'  → setSegmentSpeaker(transcriptId, name)            // newly wired
```

The call-site chain is `VirtualizedTranscriptView → TranscriptSegment →
SpeakerLabelInput`, so the `TranscriptSegment.onSpeakerSubmit` prop type also
gains `scope`, and the two `VirtualizedTranscriptView` call sites (lines 364,
429) thread both `segment.id` and `scope`. `TranscriptView.tsx:326` is a
direct closure: `(name, scope) => handleSpeakerSubmit(transcript.id,
transcript.speaker!, name, scope)`. `useSpeakerRename.ts` must add
`setSegmentSpeaker` to its import.

After either dispatch succeeds, the hook calls `setEditingSegmentId(null)` and
`onSpeakersChanged?.()` to refetch — same teardown as today.

### D3 — Reuse the revert path, with one documented edge case

A per-turn override on a **previously-labeled** row sets `previous_label` to
the old cluster label, so the existing cluster-level revert
(`revert_speaker_label`, `speaker.rs:328`) restores it. No new revert command
(DRY).

**Edge case (documented, not fixed here):** a per-turn override on a row that
had `speaker_label = NULL` (e.g., diarization was skipped, or after a full
reset with no audio) sets `previous_label = NULL` (the old `speaker_label`).
`revert_speaker_label`'s `WHERE previous_label IS NOT NULL` then excludes
that row → the undo icon (shown because the label is not "Speaker N") does
nothing. This is rare (the cluster path is the normal way to label a
never-labeled row) and is documented as a known limitation in the delta spec
plus a cargo test proving the non-revertible state. Fixing it would require
threading `previous_label` to the frontend to gate the undo icon — out of
scope (YAGNI until reported).

### D4 — Hexagonal boundary

No new hexagonal boundary crossed:
- **Port / command:** `set_segment_speaker` already exists. No new port method.
- **Adapter:** `setSegmentSpeaker` already exists. No new adapter function.
- **Use case:** `handleSpeakerSubmit` gains a branch — the legitimate extension
  point.
- **Domain:** no domain type changes.

The change wires an existing adapter into an existing use-case branch via a new
UI affordance.

### D5 — Adversarial / edge cases (§4)

| Category | Test |
|---|---|
| Wiring (segment mode) | Checkbox unchecked + name + Enter → `set_segment_speaker` dispatched with `transcriptId`; `label_speaker` NOT dispatched |
| Wiring (cluster default) | Checkbox checked (default) + name + Enter → `label_speaker` dispatched; `set_segment_speaker` NOT dispatched |
| Revert (previously-labeled row) | A per-turn-overridden previously-labeled row reverts to its own `previous_label` via the existing badge undo |
| Revert (never-labeled row — known limitation) | A per-turn override on a `speaker_label IS NULL` row is NOT reverted by `revert_speaker_label` (returns 0 rows) — documented limitation, cargo test |
| SQL injection | Name `'; DROP TABLE transcripts; --` is bound as a parameter via `sqlx` `?` placeholder; no mutation. (Defense is parameterized binding, not `sanitize_speaker_name` — see D7.) |
| Non-existent `transcript_id` | `set_segment_speaker` with an unknown id returns `Ok(false)` (0 rows); cargo test. Frontend MAY surface a toast. |
| Empty / whitespace name | No-op (input's existing "Name required" guard) |
| Suggestion chip in segment mode | Chip click with checkbox unchecked → `set_segment_speaker` with the chip name |

### D6 — TranscriptView color fix (brought in-scope)

`TranscriptView.tsx:333` derives `colorIndex` via
`parseInt(transcript.speaker.replace("Speaker ",""))`, which is `NaN` for
named labels, collapsing every named speaker to color index 0 (golden-angle
red). `VirtualizedTranscriptView.tsx:273` instead builds a `speakerIndexMap`
(first-appearance order) that works for any string.

The per-turn path makes the TranscriptView bug worse: a user can produce
adjacent "Speaker 2" (correct color) and "Carlos" (wrong, always-red)
segments in the same view — a visible relative-color regression this change
creates (cluster rename doesn't surface it as harshly because it relabels all
rows together). The fix is small and borrowed from the sibling view: build a
`speakerIndexMap` from the transcripts list (a `useMemo`) and look up each
speaker's index. This fixes named-label coloring for BOTH cluster rename and
per-turn override in TranscriptView.

**Verify during apply** which of the two views is the live render path; if
`TranscriptView` is legacy/unused, narrow the fix to the live view and file
the other separately.

### D7 — Security-claim accuracy

`sanitize_speaker_name` (`commands.rs:268`) trims, length-checks, and strips
HTML tags. It does **NOT** reject SQL-injection strings — the existing test
(`commands.rs:979`) proves `"'; DROP TABLE speakers; --"` passes through
unchanged. The actual SQL-injection defense is `sqlx` parameterized binding
(`?` placeholders in `speaker.rs`). The delta spec credits parameterized
binding, not the sanitizer, so a future refactor that switches to string
interpolation (trusting the sanitizer) would not be misled into introducing an
injection.

## Alternatives considered

- **Split-badge / dropdown:** rejected per D1.
- **Dedicated per-row override button:** clutters every row for an occasional
  action.
- **Segment as default:** regresses the common case; cluster stays default.
- **New per-row revert command:** rejected per D3 (existing cluster revert
  handles previously-labeled rows; the never-labeled edge case is documented,
  not command-added — YAGNI).

## Risks

- **Mode confusion:** users may not notice the checkbox. Mitigated by
  default-checked (= today's behavior) and an explicit label naming the
  cluster.
- **TranscriptView color fix scope:** small, but verify the live render path
  during apply (D6).
- **Never-labeled-row revert gap:** documented limitation (D3); rare in
  practice.

## Open questions

- Should the scope toggle remember the user's last choice across segments?
  Default: no (per-open state). YAGNI until reported.
- Surface a toast when `setSegmentSpeaker` returns `false` (non-existent id)?
  Nice-to-have, low priority — the refetch will simply not show the change.
