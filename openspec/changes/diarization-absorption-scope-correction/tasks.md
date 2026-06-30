## 1. Absorption characterization — diagnostic test

- [x] 1.1 **(characterization)** `test_00000001_embedding_drift_diagnostic` (`#[ignore]`,
  read-only): builds real chunks via the embedding adapter on the production recording, runs
  `cluster_by_centroids(&chunks, 0.40)`, computes per-label early-only centroids, and prints the
  geometry of the sustained absorption. NO pass/fail assertion on the cause — the cause is not yet
  understood.
- [x] 1.2 implement the diagnostic: identify the early-dominant/late-silent label (Carol
  analogue), measure her OWN late-chunk cosines to her early centroid (RULES OUT drift: measured
  cos ≈ 0.85, same-speaker range, not the ≈ 0.22 all-late-chunks mean originally mis-cited),
  record AHC faithfulness (~1 of the dominant speaker's ~234 late chunks is nearer the absorbed
  speaker's centroid), and a sequential online-centroid-tracking prototype that reproduces the
  absorption (cluster keeps ~7 late chunks). Ported to `cdbaf54`'s impl.
- [x] 1.3 **(adversarial: read-only)** the diagnostic opens the prod DB in `mode=ro`, so it
  CANNOT mutate it by construction (stronger than a snapshot assert). Verified exit 0 with no
  write attempted.

## 2. Canonical spec correction

- [x] 2.1 The delta at `specs/speaker-diarization/spec.md` (`## MODIFIED Requirements` — full
  temporal-coherence requirement block) replaces the "Absorbed speaker is recovered mid-meeting"
  scenario with "Local mis-assignment is recovered when neighbors are clean" + the out-of-scope
  blockquote, and clarifies the body parenthetical to "(local contamination recovery)". The
  out-of-scope note is grounded in the STRUCTURAL limit of any local smoothing pass (not in an
  unverified cause); drift was ruled out, root cause TBD. Reframed after the diagnostic
  disproved the drift premise; validate with `openspec` before archive.
- [x] 2.2 Confirm no other canonical spec references the withdrawn scenario (grep
  `openspec/specs/` for "Absorbed speaker is recovered" / "80 %" absorption claims). DONE: only
  `speaker-diarization/spec.md` references it (lines 521, 534-539), exactly the lines the delta
  replaces.

## 3. Narrowed regression test

- [x] 3.1 `test_temporal_coherence_regression_00000001` (`#[ignore]`): full label reset
  (`UPDATE transcripts SET speaker_label=NULL, speaker_source=NULL, previous_label=NULL
  WHERE meeting_id=…`) before re-diarization so the test measures fresh output; KEEP the flicker
  assertion (short-run singletons, `duration < 5 s`, < 10 % in min 30–70); DROP the no-absorption
  assertion (passed only on a ≥1-row technicality) and the fragment assertion (measured Whisper
  segment length — a different layer).
- [x] 3.2 port the narrowed test from the patch, adapt to `cdbaf54`, restore the prod
  DB to its pre-test state after running (mirror the existing ignored diarization tests' restore
  pattern). [restore pending — run + restore in task 4.1]

## 4. Verify + archive gate

- [x] 4.1 **(verify, `#[ignore]`)** DONE. Characterization diagnostic: drift RULED OUT (absorbed
  speaker's own late chunks cos ≈ 0.8453 to her early centroid — same-speaker range; the ≈ 0.22
  figure was the all-late-chunks mean, mis-cited). AHC faithful (~1/234). Sequential prototype
  reproduces absorption (~7 late chunks). Narrowed regression test: **min 30-70 short-singleton
  rate 1.6 % (2/127)** on `cdbaf54`'s retuned impl — the 1.6 % measured on `7637fd7` DOES carry
  over. Prod DB restored from `bak-20260630-prescope-correct` (WAL discarded); verified 545 rows
  / 238 00000001 labelled = pre-test state.
- [x] 4.2 **full gate:** `cargo test` ✓ (exit 0, integration tests green, `#[ignore]` correctly
  excluded) · `pytest backend -m "not slow"` ✓ (exit 0) · `pnpm test` ✓ (244/244, exit 0) ·
  `pnpm lint` ✓ (exit 0; one pre-existing unused-import warning, not in this change). Smoke is
  N/A — the change is backend-only (Rust tests + spec text), no IPC event, component, or
  interaction surface change.
- [x] 4.3 **before `/opsx:archive`:** re-read the delta; "Absorbed speaker is recovered
  mid-meeting" is ABSENT, "Local mis-assignment is recovered when neighbors are clean" + the
  structural out-of-scope blockquote (drift ruled out, root cause TBD) are PRESENT. `openspec
  validate` passes; 4/4 artifacts complete. The canonical spec sync happens at archive.
