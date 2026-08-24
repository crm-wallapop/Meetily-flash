## MODIFIED Requirements

### Requirement: Transcript-timestamp-driven speaker diarization runs as a post-processing queue phase

This requirement amends the canonical requirement of the same name. The canonical requirement's item 3 mandates the **effective-split chunk grid**: "Chunk each segment into pieces sized at the **effective split granularity** = `max(SPLIT_TARGET_SECS, speech_seconds / MAX_DIARIZATION_CHUNKS)` … Each piece remains within [`MIN_SPEECH_SECS`, `MAX_CHUNK_SECS`]." With this change the chunk grid is no longer the source of speaker-change-point boundaries on the success path; the in-process pyannote `ort::Session` (see the "Diarization segment granularity resolves speaker turns within Whisper segments" requirement below) supplies intra-region splits that `build_chunks` consumes INSTEAD of the effective-split grid. The canonical item 3's `effective_split` mandate is STRUCK as a boundary SOURCE on the success path; it SURVIVES in one narrow role: `build_chunks` still sub-divides any surviving segment LONGER than `MAX_CHUNK_SECS` (10s) at the effective-split granularity — which happens when a Whisper speech region has no interior pyannote change-points and therefore stays whole (e.g. a boundary-free monologue region). This is a size guard, not a competing boundary source. The only fallback remains pyannote-model-missing: when the segmentation model file is absent, `build_chunks` applies `effective_split` exactly as the canonical item 3 states, so the meeting still diarizes at coarse resolution. (There is no child-failure fallback — pyannote runs in-process via the same `ort` runtime as Parakeet, so there is no subprocess whose spawn/crash/timeout/schema-mismatch failure could fire.)

The canonical "Short meeting is unaffected by the chunk cap" scenario asserts `effective granularity equals SPLIT_TARGET_SECS (3.0 s) — unchanged from before this change`. That assertion is RE-POINTED to the in-process pyannote boundary source: on a short (~10 min) meeting the pyannote model is present (the cap is not hit), and the per-region granularity is set by the pyannote change-points inside each Whisper segment — NOT by a fixed `SPLIT_TARGET_SECS` grid. The "chunk count is identical to a fixed-3 s chunker" clause no longer holds on the success path; the chunk count on the success path equals the count of pyannote change-points (capped). On the pyannote-model-missing fallback path the canonical assertion holds unchanged.

The canonical "Long meeting does not stall in clustering" scenario asserts "the effective split granularity is coarsened so the chunk count is at or below `MAX_DIARIZATION_CHUNKS`." That cap-enforcement mechanism is RE-POINTED to pyannote-boundary shedding: on a long meeting the cap is enforced once at the pyannote-boundary layer (uniform shed every k-th candidate by position, then merge sub-`MIN_SPEECH_SECS` survivors within their Whisper region — see the "Uniform shed-to-cap" scenario below), NOT by coarsening `effective_split`. The chunk count is bounded primarily by shedding the pyannote candidate set; because `effective_split` survives as the size guard for surviving segments longer than `MAX_CHUNK_SECS`, the count reaching clustering can modestly exceed `MAX_DIARIZATION_CHUNKS` (bounded ≈2× in practice — only boundary-free >10s regions contribute extra pieces). The canonical scenario's "bounded wall-clock time" and "clustering produced N speakers from M chunks" assertions hold unchanged. On the pyannote-model-missing/corrupt fallback path the canonical `effective_split` coarsening holds unchanged.

`FINE_SPLIT_SECS` (canonical default 2.0s) is referenced by the canonical "Diarization segment granularity resolves speaker turns within Whisper segments" requirement as the turn-granularity source ("A turn of approximately 2 seconds (the fine-split granularity `FINE_SPLIT_SECS`)..."). That role is STRUCK on the success path: turn granularity is now set by the pyannote change-points, NOT by `FINE_SPLIT_SECS`. `FINE_SPLIT_SECS` SURVIVES as the `refine_pass2` re-embedding window (`build_fine_chunks` re-chunks the full recording at `FINE_SPLIT_SECS` to assign each fine chunk to its nearest Pass-1 centroid) — it is no longer the granularity-defining constant but remains the Pass-2 re-chunk cadence. (A delta that left `FINE_SPLIT_SECS` mandated as the turn-granularity source alongside a pyannote-boundary requirement would self-contradict; this note reconciles the canonical reference.)

(A delta that leaves the canonical item 3 `effective_split` mandate in place alongside a pyannote pre-splitter requirement would make the canonical spec self-contradict — both cannot be the chunk-layout source simultaneously. This amendment removes that contradiction.)

#### Scenario: Short meeting succeeds via the in-process pyannote boundary source (re-points the canonical "Short meeting" scenario)

- **GIVEN** a meeting with ~10 minutes of speech AND the pyannote segmentation model is present on disk
- **WHEN** diarization runs
- **THEN** the per-region chunk granularity is set by the pyannote change-points inside each Whisper speech region (NOT a fixed `SPLIT_TARGET_SECS` grid)
- **AND** `effective_split` is NOT applied as a boundary source on the success path (it survives only as the size guard for surviving segments longer than `MAX_CHUNK_SECS`)
- **AND** the chunk count equals the count of pyannote change-points (capped at `MAX_DIARIZATION_CHUNKS`)

#### Scenario: Short meeting falls back to the effective-split grid when the pyannote model is missing

- **GIVEN** a meeting with ~10 minutes of speech AND the pyannote segmentation model file is absent from disk
- **WHEN** diarization runs and the in-process pyannote source is unavailable
- **THEN** `build_chunks` applies the canonical effective-split grid (`SPLIT_TARGET_SECS = 3.0 s` for a short meeting) exactly as the canonical item 3 states
- **AND** the effective granularity equals `SPLIT_TARGET_SECS` (3.0 s) — the canonical assertion holds on the fallback path
- **AND** no panic propagates to the user-facing diarization flow

#### Scenario: Long meeting cap is enforced by pyannote-boundary shedding (re-points the canonical "Long meeting does not stall in clustering" scenario)

- **GIVEN** a meeting with ~83 minutes of speech whose pyannote candidate-boundary count exceeds `MAX_DIARIZATION_CHUNKS`
- **WHEN** diarization runs with the in-process pyannote boundary source
- **THEN** the cap is enforced at the pyannote-boundary layer (uniform shed every k-th candidate by position, then merge sub-`MIN_SPEECH_SECS` survivors within their Whisper region, never across a silence gap) — NOT by coarsening `effective_split`
- **AND** the segment count passed onward from shedding is at or below `MAX_DIARIZATION_CHUNKS` (the chunk count after any >`MAX_CHUNK_SECS` sub-division may modestly exceed it; see the requirement text)
- **AND** the clustering step completes in bounded wall-clock time (seconds, not minutes)
- **AND** a `clustering produced N speakers from M chunks` log line is emitted (the prior failure mode where this line never appeared is gone)

### Requirement: Diarization segment granularity resolves speaker turns within Whisper segments

Whisper groups transcript segments by sentence/VAD, not by speaker; on multi-speaker meetings these segments routinely span 15–30s and contain two or more speakers. The diarization output SHALL be granular enough that a speaker turn occurring inside a single Whisper transcript segment produces a diarization segment boundary at or near the turn, so that per-word alignment can attribute the words on each side of the turn to the correct speakers rather than collapsing the whole segment to one speaker.

Speaker change-point boundaries SHALL be sourced from a pyannote `ort::Session` running **in-process** — a second `ort::Session` (the first serves Parakeet transcription; after this change a third serves the ported nemo_titanet extractor) over `pyannote-segmentation-3.0`, the exact pattern the Phase 1 probe (`pyannote_ort_probe.rs:48-59`) validated. The segmentation + sliding-window + powerset-decode + smoothing + boundary-emission logic is the productionized form of the Phase 2b probe (`pyannote_ort_probe.rs`): slide a 10s window at 1s step over the recording's 16 kHz mono samples, decode per-frame powerset logits to 3-speaker multilabel activity via hysteresis at onset 0.5, apply pyannote-default smoothing (median filter rad=3, min_on=0.3s, max_off=0.5s — the only Phase 2b config that hit BOTH known anchors), and emit `Vec<(start_seconds, end_seconds)>` change-points. The diarization flow (`commands.rs:413-432`) SHALL INTERSECT the pyannote change-points with the Whisper `transcript_segments` (`fetch_transcript_timestamps`) — a pyannote change-point inside a Whisper speech region is kept as an intra-region split; the Whisper silence regions are preserved as silence (not embedded). The intersected set is passed as the `transcript_segments` argument to `adapter.process()`.

**Why in-process on one runtime, not a subprocess:** sherpa-onnx-sys 1.13.4 statically bundles ORT 1.17.1 (C-API ≤17); the project's `ort = "2.0.0-rc.10"` dep (used for Parakeet transcription) brings C-API 27. The two runtimes collide on the global C-API symbol table the moment both are linked into one process → STATUS_ACCESS_VIOLATION. This was verified by the `pyannote_sherpa_load_crux` probe. This change resolves the conflict at the root by **removing sherpa-onnx entirely** and porting nemo_titanet embedding extraction to the `ort` crate (see design.md D1); with sherpa gone, Parakeet + nemo_titanet + pyannote all share one ORT runtime — no conflict by construction. The port is empirically validated: the `embed-probe-ort` crate reproduces sherpa's nemo_titanet embeddings at cosine **0.9946–0.9989** on production-relevant clips (clean/overlap ≥1.5s, non-silent) after a one-line log-floor fix (`f32::MIN_POSITIVE` → `f32::EPSILON`), well within the AHC operating margin. See `openspec/exploration/diarization-pyannote-boundaries-ort-probe.md` §"ARCHITECTURE LOOP CLOSED". A subprocess/IPC/second-binary path (Option 3) was panel-rejected as permanent subprocess debt once the port proved viable.

The pyannote boundary set AUGMENTS the Whisper transcript segments with intra-region splits; it does NOT supersede or replace the Whisper boundaries (which remain the speech-vs-silence mask). After this change, `build_chunks` sub-divides each Whisper speech region by the pyannote boundaries inside it and no longer applies `effective_split` as a boundary SOURCE (`sherpa_adapter.rs`); the only residual use of `effective_split` is the size guard that sub-divides surviving segments longer than `MAX_CHUNK_SECS`. The `MAX_DIARIZATION_CHUNKS` cap is enforced once, at the pyannote-boundary layer (see the uniform-shed scenario below). (A proposal that leaves both the uniform-grid step and the pyannote pre-splitter mandated as boundary sources simultaneously is NON-CONFORMANT — the canonical spec would contradict itself.)

The pyannote `ort::Session` emits boundaries only — no speaker labels, no embeddings (the session is over pyannote-segmentation-3.0 only). This is stronger than "labels discarded": there is nothing to discard. Meetily's AHC clustering, label-quality refinement, most-isolated-cluster cap, temporal-coherence smoothing, and cross-meeting registry matching remain authoritative for labeling, exactly as today.

#### Scenario: Sub-turn interjection is isolated, not swallowed

- **GIVEN** a Whisper transcript segment from 46:58 to 47:21 containing a 2s Ricardo interjection at 46:58–47:00 followed by Cynthia's speech
- **AND** the production diarization previously labeled the entire 46:58–47:30 run as Cynthia
- **WHEN** diarization runs with the in-process pyannote boundary source
- **THEN** the diarization output contains a speaker segment boundary near 47:00 separating Ricardo (≈46:50–47:00) from Cynthia (≈47:00 onward), so the interjection's words are attributed to Ricardo
- **AND** the chunk-grid-only baseline over the same window does not produce that boundary

#### Scenario: Back-and-forth between two speakers is not collapsed to one

- **GIVEN** a region where two speakers alternate in 4–8s turns across a 30s window
- **WHEN** diarization runs with the in-process pyannote boundary source
- **THEN** the output preserves the alternation as multiple segments rather than merging the window into a single speaker's run

#### Scenario: Single-speaker meeting is not fragmented

- **GIVEN** a meeting with exactly one speaker
- **WHEN** diarization runs with the in-process pyannote boundary source
- **THEN** the output is a single speaker (no spurious second cluster introduced by the finer boundary placement)

#### Scenario: Pyannote-model-missing falls back to the effective-split grid

- **GIVEN** the pyannote segmentation model file is absent from disk (not downloaded, or deleted)
- **WHEN** diarization runs and the in-process pyannote session cannot be constructed
- **THEN** the diarization proceeds with the canonical effective-split (`SPLIT_TARGET_SECS`) grid as the `transcript_segments` subdivision source
- **AND** the meeting still diarizes (at coarse resolution); only the finer pyannote boundaries are lost
- **AND** no panic propagates to the user-facing diarization flow

#### Scenario: Uniform shed-to-cap still recovers alternation turns on long meetings

- **GIVEN** a long (≥45 min) meeting with a rapid two-speaker alternation region and a single-speaker monologue region of comparable length
- **WHEN** the candidate-boundary count exceeds `MAX_DIARIZATION_CHUNKS` and uniform shedding runs (every k-th by position), followed by Meetily's AHC + temporal-coherence smoothing
- **THEN** the alternation region's turn structure is recovered (a threshold fraction of within-region turns are preserved in the final labeling), because turns are re-derived from the surviving candidate set, not carried by individual shed boundaries
- **AND** the resulting segment count after shedding is at or below `MAX_DIARIZATION_CHUNKS`

#### Scenario: Silent or empty audio does not crash the in-process flow

- **GIVEN** a silent or empty audio fixture
- **WHEN** the in-process pyannote session runs and yields an empty boundary set
- **THEN** the diarization proceeds (with an empty intersected set or the effective-split fallback) without panicking

#### Scenario: Corrupt-but-present pyannote model falls back to the effective-split grid

- **GIVEN** the pyannote segmentation model file is PRESENT on disk but corrupt (truncated, bad magic, or yields non-finite output mid-decode)
- **WHEN** the in-process pyannote `ort::Session` construction errors OR inference produces NaN/Inf
- **THEN** the diarization falls back to the canonical effective-split grid (the same fallback path as model-missing)
- **AND** the meeting still diarizes at coarse resolution (≥1 labeled `SpeakerSegment`)
- **AND** no panic propagates to the user-facing diarization flow

#### Scenario: A pyannote change-point exactly on a Whisper segment edge produces no zero-length split

- **GIVEN** a pyannote change-point whose timestamp coincides exactly with a Whisper `transcript_segment` start or end
- **WHEN** the intersect step runs
- **THEN** no zero-length split is emitted (the intersect SHALL deduplicate/clamp so every intra-region split has positive duration ≥ `MIN_SPEECH_SECS`, or is dropped)
- **AND** no `Chunk` with `duration_secs < MIN_SPEECH_SECS` reaches `adapter.process()`

#### Scenario: ort::Session wrapping preserves Send+Sync and clustering runs off the async executor

- **GIVEN** `ort::Session` is `Send + Sync` (ort 2.0.0-rc.10) and the port wraps it in `Mutex<Session>` (design D1) or a session-pool fallback
- **WHEN** the diarization `process()` runs
- **THEN** the wrapping remains `Send + Sync` so extraction + clustering execute on a blocking thread (per the canonical "Clustering does not freeze the UI" requirement), NOT on the async executor
- **AND** the async runtime and UI remain responsive during the diarization pass

#### Scenario: Concurrent multi-meeting diarization is isolated

- **GIVEN** N (≥2) meetings diarized concurrently, sharing the process's ort sessions (Parakeet + nemo_titanet + pyannote)
- **WHEN** their diarization passes interleave on the shared sessions
- **THEN** each meeting produces correct per-meeting results with no cross-meeting state leakage
- **AND** the shared-session contract is documented: either meetings serialize on the `Mutex<Session>` lock (no extraction interleaving across meetings) or each meeting gets an isolated session clone (memory cost)
- **AND** the shared registry (`HashMap<String, Vec<Vec<f32>>>`) does not corrupt under concurrent append (no panic, no wrong-label bleed across meetings)

## ADDED Requirements

### Requirement: The pyannote segmentation model is actually consumed by the in-process ort::Session

The pyannote-segmentation ONNX model SHALL be loaded and run by an in-process `ort::Session` (the second `ort` session in the process, alongside the Parakeet and nemo_titanet sessions) — NOT by a child binary or sherpa's `OfflineSpeakerDiarization` (which is non-viable due to the ORT runtime conflict). The session SHALL emit a deterministic, non-empty boundary set on real multi-speaker audio when the model is present, and SHALL change behavior (construction error or distinct/empty segmentation output) when the model file is swapped for a committed dummy fixture. This closes the prior phantom-dependency state where `segmentation_model_path` was accepted by the adapter constructor, existence-checked, and discarded.

#### Scenario: The in-process session loads and runs the segmentation model

- **GIVEN** the in-process pyannote `ort::Session` is constructed with a `model_dir` pointing at the on-disk pyannote model
- **WHEN** the session runs inference on a real multi-speaker clip
- **THEN** the emitted boundary set is deterministic and non-empty
- **AND** swapping the model file for a committed dummy fixture changes the session's behavior (construction error or distinct segmentation output — presence-of-path alone is not sufficient evidence of consumption)

### Requirement: nemo_titanet embedding extraction is ported to ort and sherpa-onnx is removed

The nemo_titanet embedding extraction SHALL be performed by an in-process `ort::Session` (the ported `NemoEmbeddingExtractor`, lifting the validated `embed-probe-ort` fbank + CMVN + pad-16 + transpose + session-builder pipeline) — NOT by sherpa-onnx's `SpeakerEmbeddingExtractor`. The `SpeakerEmbeddingManager` SHALL be replaced by a pure-Rust in-memory cosine store (`HashMap<String, Vec<Vec<f32>>>` + cosine search; sherpa's manager was a convenience wrapper, not a model). The `search` operation SHALL be a per-vector best-score scan — iterate every stored vector across all names and return the name of the single highest-cosine vector ≥ threshold — matching sherpa's `SpeakerEmbeddingManager::search` semantics exactly, NOT a per-speaker-centroid search (a centroid search would diverge when a speaker has one near-query vector and one far vector; the per-vector scan lets the near vector win). sherpa-onnx and sherpa-onnx-sys SHALL be removed from `Cargo.toml`, so the whole app links exactly one ORT runtime (the `ort` crate) and the C-API 17-vs-27 collision that motivated this change cannot occur by construction. The stored `speaker_embeddings` vectors remain nemo_titanet 192-dim — no schema migration. Registry hydration (`database/setup.rs`) SHALL construct the store at `dim = 192` (or read `dim()` from the extractor) — NOT the hardcoded `dim = 256` that silently loads zero speakers today (pre-existing bug fixed by this change).

#### Scenario: sherpa-onnx is no longer in the production dependency graph

- **GIVEN** the port is complete and `Cargo.toml` no longer declares `sherpa-onnx`
- **WHEN** `cargo tree -p meetily-flash` is run (scoped to the `meetily-flash` crate — NOT workspace root, because `embed-probe-sherpa` remains a workspace member as the cosine-gate reference binary, so workspace-root `cargo tree` still transitively shows sherpa)
- **THEN** neither `sherpa-onnx` nor `sherpa-onnx-sys` appears in the `meetily-flash` dependency graph
- **AND** a grep for `sherpa_onnx::` AND `SherpaOnnx` across BOTH `frontend/src-tauri/src/` AND `frontend/src-tauri/tests/` returns zero hits (the port replaced every sherpa reference in the speaker module, commands, state, database setup, smoke test, and the integration/probe tests)

#### Scenario: The port reproduces sherpa's embeddings within the AHC operating margin

- **GIVEN** the fixed 10-clip gate set plus production-representative additions, each clip ≥ 1.5s and passing `is_effectively_silent`, INCLUDING ≥4 clips uniformly distributed in [1.5, 3.0]s (the production pyannote-chunk regime) AND ≥2 clips at exactly 2.0s (the `refine_pass2` / `FINE_SPLIT_SECS` re-embedding window) — regimes that reach clustering, NOT dropped inputs
- **WHEN** the ported `NemoEmbeddingExtractor` and the sherpa reference extract embeddings from the same 16kHz mono clip
- **THEN** the cosine similarity between the two embeddings meets the margin-derived tiered threshold: ≥ 0.99 for clips ≥ 2.0s and ≥ 0.98 for clips in [1.5, 2.0)s — the floors are derived from the AHC separation margin (merge 0.40, inter-speaker cosine 0.6–0.8; measured residual worst-case 0.0131 is ~46× below the 0.60 inter-speaker floor), and SHALL be revised ONLY if that downstream margin changes — never in response to a failing measurement
- **AND** the per-clip cosine is reported (not just an aggregate pass/fail), so a regression in the 1.5–3s or 2.0s regime is visible
- **AND** the gate is re-run in full on any ORT-kernel upgrade (the drift-tripwire role — the bar guards future drift; AHC parity certifies the current port)
- **AND** before the gate is final: (a) ≥10 diverse-speaker 1.5s clips pass with worst-case ≥ 0.98 (tail evidence); (b) noise-injection invariance — reference embeddings perturbed by the measured worst-case residual (0.013) yield identical AHC clusterings
- **AND** filter parity holds — the port drops (via `is_effectively_silent`, `is_ready` / the minimum-frame gate, and `MIN_SPEECH_SECS`) exactly the clips sherpa drops, verified on a 25ms→2s sweep (not just the known cases)

**Speaker-attributed segment overlap (the parity metric).** For a reference labeling `ref` and a new labeling `new` over the same recording, for each speaker label `L` present in `ref`: `overlap(L) = |ref_segments(L) ∩ new_segments_same_speaker(L)| / |ref_segments(L)|`, where `ref_segments(L)` is the set of reference segments labeled `L` measured in seconds of audio, `new_segments_same_speaker(L)` is the set of new-run segments labeled with `L`'s corresponding label (labels matched across the two runs by Hungarian assignment on per-label segment-time overlap, to handle renumbering), and `∩` is temporal intersection in seconds. The score is the unweighted mean of `overlap(L)` over all labels `L` in `ref`. The per-label `overlap(L)` SHALL be reported (not just the mean), so a single collapsed speaker is visible rather than hidden in an aggregate.

#### Scenario: Extractor-only parity vs the sherpa reference (load-bearing)

- **GIVEN** committed multi-speaker fixtures
- **WHEN** the diarization runs TWICE with the SAME boundary source (`effective_split` grid — the pre-boundary-change chunk layout) but DIFFERENT extractors: once with the ported `NemoEmbeddingExtractor`, once with the sherpa extractor
- **THEN** the resulting cluster counts are identical
- **AND** speaker-attributed segment overlap (per the metric above) is ≥ 0.95, reported per-label
- **AND** this gate runs UNCONDITIONALLY on committed fixtures (NOT `#[ignore]`) — it isolates the extractor port (the change cosine was always a proxy for) from the boundary change

#### Scenario: Boundary-acceptance parity (confirmation)

- **GIVEN** ≥10 labeled multi-speaker recordings AND the pyannote model present
- **WHEN** the diarization runs TWICE with the SAME (ported) extractor but DIFFERENT boundary sources: once with pyannote boundaries, once with the `effective_split` grid
- **THEN** the resulting cluster counts are identical (the boundary change re-segments but AHC + smoothing recover the same speakers)
- **AND** speaker-attributed segment overlap (per the metric above) is ≥ 0.95, reported per-label (pyannote boundaries should match or improve overlap, not regress it)
