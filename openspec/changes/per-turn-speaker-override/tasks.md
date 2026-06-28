# Tasks

## 1. Scope toggle in `SpeakerLabelInput`

- [x] 1.1 **(red)** Smoke: open the speaker input; assert the scope checkbox is
  visible, checked by default, and labeled with the current cluster label.
  Fails: checkbox doesn't exist.
- [x] 1.2 **(red → wiring: cluster default, no regression)** with the checkbox
  checked (default), type a name + Enter → assert `label_speaker` dispatched
  with `(meetingId, clusterLabel, name)` AND `set_segment_speaker` NOT
  dispatched.
- [x] 1.3 **(red → wiring: segment mode)** uncheck the box, type a name + Enter
  → assert `set_segment_speaker` dispatched with `(transcriptId, name)` AND
  `label_speaker` NOT dispatched. **Requires the mock fix in task 5 first**
  (the `set_segment_speaker` smoke mock currently does not record dispatch).
- [x] 1.4 **(red → adversarial: suggestion chip respects scope)** uncheck the
  box, click a suggestion chip → assert `set_segment_speaker` (not
  `label_speaker`) dispatched with the chip name. **Requires `__smokeSpeakers`
  fixture** (named speakers) so chips render.
- [x] 1.5 **(green)** Add the scope checkbox to `SpeakerLabelInput`
  (`SpeakerBadge.tsx`), local `useState<'cluster' | 'segment'>('cluster')`,
  label interpolates the current cluster label, pass `scope` through
  `onSubmit(name, scope)`. Tests 1.1–1.4 pass.

## 2. Hook branching + call-site threading (incl. TranscriptSegment ripple)

- [x] 2.1 **(red)** `handleSpeakerSubmit(transcriptId, clusterLabel, name,
  scope)` with `scope='segment'` dispatches `setSegmentSpeaker(transcriptId,
  name)`. Assert via the smoke capture (after task 5).
- [x] 2.2 **(green)** Add `setSegmentSpeaker` to `useSpeakerRename.ts` imports.
  Extend `handleSpeakerSubmit` to branch: `'cluster'` → `labelSpeaker`
  (unchanged), `'segment'` → `setSegmentSpeaker`. Both share
  `setEditingSegmentId(null)` + `onSpeakersChanged?.()` teardown.
- [x] 2.3 **(green)** Update the `TranscriptSegment.onSpeakerSubmit` prop type
  to `(name: string, scope: 'cluster' | 'segment') => void`; thread `segment.id`
  and `scope` through the two `VirtualizedTranscriptView` call sites (lines 364,
  429). Update the `TranscriptView.tsx:326` closure to
  `(name, scope) => handleSpeakerSubmit(transcript.id, transcript.speaker!, name, scope)`.

## 3. TranscriptView color fix (D6)

- [x] 3.1 **(red)** After a cluster rename to "Alice" in `TranscriptView`,
  the renamed badge color differs from a "Speaker 0" badge (currently both
  collapse to color index 0 via the `parseInt` NaN). Verify which view is the
  live render path first.
- [x] 3.2 **(green)** Replace the `parseInt(transcript.speaker.replace(...))`
  `colorIndex` derivation in `TranscriptView.tsx:333` with a `speakerIndexMap`
  (first-appearance order, `useMemo`) mirroring
  `VirtualizedTranscriptView.tsx:273`. Named labels now get distinct, correct
  colors in both views.

## 4. Repository-level guarantees (backend is shipped; prove the wiring honors it)

- [x] 4.1 **(red → adversarial: SQL injection)** `setSegmentSpeaker(transcriptId,
  "'; DROP TABLE transcripts; --")` is bound via `sqlx` `?` placeholder; no
  mutation, no table dropped. Cargo test (defense is parameterized binding, not
  `sanitize_speaker_name`).
- [x] 4.2 **(red → adversarial: non-existent transcript_id)**
  `set_segment_speaker` with an unknown id returns `Ok(false)` (0 rows). Cargo
  test. Frontend MAY surface a toast on `false` (low priority).
- [x] 4.3 **(red → adversarial: never-labeled row revert gap)** a per-turn
  override on a `speaker_label IS NULL` row sets `previous_label = NULL`;
  `revert_speaker_label` then returns 0 rows for it (documented limitation per
  design D3). Cargo test proving the non-revertible state.
- [x] 4.4 **(note, not a new test)** the existing
  `auto_label_does_not_overwrite_manual` test (`commands.rs:1319`) already
  covers the only real survival path (auto-write won't clobber manual within a
  non-reset run). Do NOT write a "survives reset_speaker_labels" test — that
  would be false (the Speakers button clears all manual labels per the
  canonical spec).
- [x] 4.5 **(added during review round-1 — MAJOR finding #2)** the set-once
  CASE invariant on the previously-labeled path: a second
  `update_transcript_speaker_manual` takes the ELSE branch so revert restores
  the ORIGINAL cluster label, not an intermediate manual name. Cargo test
  `manual_override_sets_previous_label_exactly_once_on_previously_labeled_row`
  (4.3's never-labeled path can't reach this branch).

## 5. Smoke mock wiring

- [x] 5.1 **(green)** In `frontend/e2e/smoke/_meeting-details.ts`, change the
  `set_segment_speaker` mock registration to push
  `{ cmd: 'set_segment_speaker', transcriptId: args.transcriptId, speakerLabel:
  args.speakerLabel }` into `__smokeSpeakerCalls` (currently a no-op returning
  `true`). Required for tasks 1.3, 1.4, 2.1 to assert dispatch.

## 6. Spec update + archive gate

- [x] 6.1 Update `openspec/specs/speaker-diarization/spec.md` — add the
  per-turn override requirement per this change's delta spec (amending the
  "Retroactive speaker labeling" requirement's scope).
- [x] 6.2 **Before `/opsx:archive`:** re-read `specs/speaker-diarization/spec.md`
  and `design.md`; amend if the implementation evolved during apply. Confirm
  the live render path for the D6 color fix.
- [x] 6.3 Run the merge gate: `cargo test && pytest && pnpm test && pnpm lint`.
  Smoke IS required — add `frontend/e2e/smoke/per-turn-speaker-override.spec.ts`
  covering 1.2, 1.3, 1.4 via the event-bus mock (the pre-push hook derives the
  filename from `enhance/per-turn-speaker-override`).
