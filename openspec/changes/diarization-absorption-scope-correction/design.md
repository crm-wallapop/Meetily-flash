## Context

The archived `2026-06-29-diarization-temporal-coherence` change shipped a temporal-coherence
smoothing pass and synced a **"Absorbed speaker is recovered mid-meeting"** scenario into the
canonical `speaker-diarization` spec (≥80% recovery, "C's cluster does not vanish"). On the very
production meeting that motivated the change (`meeting-00000001-…`, 3 speakers, 70 min), that
claim does not hold: a speaker is still absorbed from minute 30 onward. A read-only diagnostic
was run to establish why; it **ruled out** the embedding-drift hypothesis originally suspected
(the absorbed speaker's OWN late chunks are cos ≈ 0.85 to her early centroid — same-speaker
range; the ≈ 0.22 figure cited earlier was a misread of the mean cosine of ALL late chunks to
her centroid). The root cause is not yet determined. The canonical spec is nonetheless making a
claim the system cannot deliver: the label-level smoothing **structurally** cannot recover a
sustained regional mis-assignment, so the claim must be corrected regardless of cause. The
production regression test was passing the no-absorption assertion on a ≥1-row technicality.

This change corrects the spec to the truth, commits the diagnostic as executable evidence, and
narrows the regression test to the single assertion that is actually verified (flicker). The
smoothing implementation on `main` (commit `cdbaf54`, retuned constants) is **unchanged** — this
is a spec-truth and test-truth correction, not a behavior change.

## Goals / Non-Goals

**Goals:**
- Make the canonical spec honest: replace the false sustained-absorption-recovery scenario with
  a local-recovery scenario + an explicit out-of-scope note grounded in the *structural* limit of
  any local smoothing pass (not in an unverified cause).
- Commit the absorption characterization as a `#[ignore]` read-only test so a future change does
  not re-attempt a label-level fix before establishing the cause — and so the disproved drift
  hypothesis is not re-derived.
- Narrow the 00000001 regression test to the verified flicker assertion.

**Non-Goals:**
- Fixing sustained absorption — its root cause is not yet determined (drift was ruled out;
  over-merge into a neighboring cluster is suspected but unconfirmed). A separate change that
  first establishes the cause is the correct venue.
- Changing the smoothing pass, its constants (`self_weight` 0.6, margin 0.03), or its insertion
  point.
- Any UI, port, storage, or migration change.

## Decisions

### D1 — Withdraw the scenario, do not soften it

The "Absorbed speaker is recovered mid-meeting" scenario is **removed** and replaced with "Local
mis-assignment is recovered when neighbors are clean", plus a blockquote stating sustained
absorption is out of scope as a STRUCTURAL limit of any local smoothing pass (a neighborhood vote
reinforces a regional consensus). Softening (e.g., lowering the 80% threshold) was rejected —
there is no threshold at which the smoothing recovers sustained absorption, because every temporal
neighbor carries the same (wrong) label and the neighborhood vote reinforces it. The out-of-scope
note deliberately does NOT assert a cause: the drift hypothesis was tested and ruled out (cos ≈
0.85 to her own early centroid — same-speaker range), and the root cause is filed separately.
The requirement body's "(absorption recovery)" parenthetical is clarified to
"(local contamination recovery)" so the body and scenario agree on scope.

### D2 — Diagnostic is a `#[ignore]` characterization test, not a doc note

The absorption characterization lives as `test_00000001_embedding_drift_diagnostic` in
`commands.rs`, gated `#[ignore]` (it builds real chunks via the embedding adapter against the
production recording and reads the prod DB). It is **read-only** — no DB mutation (opens the pool
in `mode=ro`, so it cannot write even if a query were added). It records the geometry of the
sustained absorption three ways: (a) the absorbed speaker's OWN late chunks are cos ≈ 0.85 to
her early centroid — **same-speaker range, ruling out** the drift hypothesis (the ≈ 0.22 figure
originally suspected was a misread of the mean cosine of ALL late chunks to her centroid, low
only because most late chunks belong to other speakers); (b) the global AHC is faithful to its
own centroids (only ~1 of the dominant speaker's 234 late chunks is nearer the absorbed
speaker's centroid); (c) a sequential online-centroid-tracking prototype reproduces the
absorption (the absorbed speaker's cluster keeps ~7 late chunks), showing the obvious
clustering-level alternative does not recover her either. The test carries NO pass/fail
assertion on these values — it is characterization that prints its findings, so it documents
what is true today without freezing a not-yet-understood cause into an assertion. An executable
test is preferred over a doc note because it reruns automatically when diarization code changes
and surfaces any shift in the absorption's geometry.

### D3 — Narrow the regression test, keep flicker

The 00000001 regression test keeps its verified flicker assertion (short-run singletons < 10% in
min 30–70). It drops the no-absorption assertion (passed only on a ≥1-row technicality) and the
fragment assertion (which measured Whisper segment length — a different layer diarization cannot
change). It adds a full label reset (`speaker_label`/`speaker_source`/`previous_label` cleared)
before re-diarization so it measures fresh output, not stale manual labels — mirroring the
Speakers-button `reset_speaker_labels` path.

### D4 — Port from the 7637fd7 impl to cdbaf54's retuned impl

The diagnostic and narrowed test were authored against the parallel `7637fd7` impl
(`temporal_coherence_fixpoint`). They are ported to `main`'s `cdbaf54` impl
(`smooth_to_fixed_point`, `SMOOTH_SELF_WEIGHT = 0.6`). Both use the same public seam
(`adapter.build_chunks`, `cluster_by_centroids(&chunks, 0.40)`), so the port is mechanical, but
the flicker measurement must be **re-run on the retuned impl** — the 1.6 % figure was measured on
`7637fd7` and is not assumed to carry over exactly. The assertion threshold (< 10 %) is
conservative either way.

## Risks / Trade-offs

- **Diagnostic touches the prod DB** → read-only by construction (opens the pool in `mode=ro`,
  so it cannot write even if a query were added later). Run against a backup if paranoid.
- **Regression test mutates speaker labels** (full reset + re-diarization) → it must restore the
  DB to its pre-test state, mirroring the existing ignored diarization tests' restore pattern.
- **Flicker figure may differ on the retuned impl** → re-verify on `cdbaf54`; do not cite 1.6 %
  for the retuned impl until measured.
- **Spec correction does not improve the user-visible absorption failure** → accepted; the user
  decision (2026-06-29) was to ship the retuned impl as-is and correct the claim in a follow-up
  rather than block on it. The absorption failure remains open; its root cause is not yet
  determined (drift ruled out, over-merge suspected) and is filed for a separate change.
