use anyhow::{anyhow, Result};
use rayon::prelude::*;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use super::diarization::{DiarizationOutput, DiarizationPort};
use super::nemo_extractor::NemoEmbeddingExtractor;
use super::registry::SpeakerIdentificationPort;
use super::types::{EmbeddingVector, SpeakerSegment};

/// nemo_titanet embedding extraction + AHC diarization pipeline, running
/// entirely on the project's `ort` runtime (Part B: sherpa-onnx removed).
pub struct OrtDiarizationAdapter {
    extractor: NemoEmbeddingExtractor,
    merge_threshold_fp: Arc<AtomicU32>,
}

impl OrtDiarizationAdapter {
    pub fn new(model_path: &str, segmentation_model_path: &str) -> Result<Self> {
        Self::with_shared_threshold(model_path, segmentation_model_path, Arc::new(AtomicU32::new(to_fp(MERGE_SIMILARITY_DEFAULT))))
    }

    pub fn with_shared_threshold(
        model_path: &str,
        segmentation_model_path: &str,
        threshold_fp: Arc<AtomicU32>,
    ) -> Result<Self> {
        let mp = PathBuf::from(model_path);
        if !mp.exists() {
            return Err(anyhow!("diarization model not found: {}", model_path));
        }
        let sp = PathBuf::from(segmentation_model_path);
        if !sp.exists() {
            return Err(anyhow!("segmentation model not found: {}", segmentation_model_path));
        }

        // nemo_titanet via `ort` (Part B port — replaces sherpa's extractor;
        // validated at 0.9946–0.9989 cosine on production-relevant clips).
        let extractor = NemoEmbeddingExtractor::new(model_path)?;

        Ok(Self {
            extractor,
            merge_threshold_fp: threshold_fp,
        })
    }

    fn merge_threshold(&self) -> f32 {
        from_fp(self.merge_threshold_fp.load(Ordering::Relaxed))
    }
}

const MERGE_SIMILARITY_DEFAULT: f32 = 0.40;

const MIN_SPEECH_SECS: f64 = 1.5;
const MAX_CHUNK_SECS: f64 = 10.0;
const SPLIT_TARGET_SECS: f64 = 3.0;
// Ceiling on chunk count for long meetings. The split granularity coarsens
// above 3 s once total speech exceeds 600 × 3 s = 30 min, keeping n bounded so
// the O(n²) clustering stays sub-second (design D2).
const MAX_DIARIZATION_CHUNKS: usize = 600;

/// The diarization chunk cap (Part B: enforced at the pyannote-boundary layer,
/// design D4 — see `pyannote_segmentation::shed_boundaries_to_cap`).
pub(crate) fn max_diarization_chunks() -> usize {
    MAX_DIARIZATION_CHUNKS
}
const MIN_CLUSTER_FRAC: f64 = 0.02;

// Pass-2 fine re-chunking granularity (design D2). Uniform across the full
// recording, independent of Whisper segment boundaries — unlike build_chunks,
// which respects MAX_CHUNK_SECS and leaves ≤10s segments as a single chunk.
// Sub-turn interjections (≥2s) only resolve if every region is sub-divided.
pub(crate) const FINE_SPLIT_SECS: f64 = 2.0;

fn to_fp(v: f32) -> u32 {
    (v * 65536.0) as u32
}

fn from_fp(v: u32) -> f32 {
    v as f32 / 65536.0
}

pub(crate) struct Chunk {
    pub(crate) start_sample: usize,
    pub(crate) end_sample: usize,
    pub(crate) duration_secs: f64,
    pub(crate) embedding: Vec<f32>,
}

impl DiarizationPort for OrtDiarizationAdapter {
    fn process(&self, samples: &[f32], sample_rate: u32, segments: &[(f64, f64)]) -> Result<DiarizationOutput> {
        if samples.is_empty() {
            return Ok(DiarizationOutput { segments: vec![], centroids: HashMap::new() });
        }

        let sr = sample_rate as usize;
        let sr_f = sr as f64;
        let duration_secs = samples.len() as f64 / sr_f;

        // If no transcript segments provided, fall back to energy-based VAD.
        let segments = if segments.is_empty() {
            log::warn!("DIARIZATION: no transcript segments, falling back to energy VAD");
            energy_vad_segments(samples, sample_rate)
        } else {
            segments.to_vec()
        };

        if segments.is_empty() {
            return Ok(DiarizationOutput {
                segments: vec![SpeakerSegment {
                    start_seconds: 0.0,
                    end_seconds: duration_secs,
                    speaker_id: 0,
                }],
                centroids: HashMap::new(),
            });
        }

        // Step 1: Create audio chunks from transcript segments.
        let chunks = self.build_chunks(samples, sample_rate, &segments);

        if chunks.is_empty() {
            return Ok(DiarizationOutput {
                segments: vec![SpeakerSegment {
                    start_seconds: 0.0,
                    end_seconds: duration_secs,
                    speaker_id: 0,
                }],
                centroids: HashMap::new(),
            });
        }

        // Step 2: Centroid-based agglomerative clustering.
        let t_cluster = std::time::Instant::now();
        let threshold = self.merge_threshold();
        let (labels, cluster_centroids) = cluster_by_centroids(&chunks, threshold);
        let n_clusters: std::collections::HashSet<u32> = labels.iter().copied().collect();
        log::info!(
            "DIARIZATION: clustering produced {} speakers from {} chunks in {:.2}s (threshold={:.2})",
            n_clusters.len(),
            chunks.len(),
            t_cluster.elapsed().as_secs_f64(),
            threshold,
        );

        // Temporal-coherence smoothing (change: diarization-temporal-coherence):
        // de-contaminate per-chunk labels via a neighbourhood vote before
        // coalescing, so a contamination seed cannot drift and absorb a real
        // speaker mid-meeting. Runs inside process() so enforce_max_speakers_cap
        // (in commands.rs) judges isolation on these de-contaminated centroids.
        let (labels, cluster_centroids) = {
            let embeddings: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
            let timestamps: Vec<f64> =
                chunks.iter().map(|c| c.start_sample as f64 / sr_f).collect();
            let durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();
            smooth_to_fixed_point(
                &labels,
                &embeddings,
                &timestamps,
                &durations,
                &cluster_centroids,
                &SmoothParams::default(),
            )
        };
        let n_clusters: std::collections::HashSet<u32> = labels.iter().copied().collect();
        log::info!(
            "DIARIZATION: temporal smoothing produced {} speakers",
            n_clusters.len(),
        );

        // Step 3: Build speaker segments from clustered chunks.
        // Sort chunks by start_sample to get temporal order.
        let mut indexed: Vec<(usize, u32)> = labels.into_iter().enumerate().collect();
        indexed.sort_by_key(|(i, _)| chunks[*i].start_sample);

        let mut result: Vec<SpeakerSegment> = Vec::new();
        if indexed.is_empty() {
            return Ok(DiarizationOutput { segments: result, centroids: HashMap::new() });
        }

        let mut cur_speaker = indexed[0].1;
        let mut seg_start = chunks[indexed[0].0].start_sample as f64 / sr_f;
        let mut seg_end = chunks[indexed[0].0].end_sample as f64 / sr_f;

        for (chunk_idx, label) in &indexed[1..] {
            let chunk = &chunks[*chunk_idx];
            let chunk_start = chunk.start_sample as f64 / sr_f;
            let chunk_end = chunk.end_sample as f64 / sr_f;

            if *label == cur_speaker {
                seg_end = chunk_end;
            } else {
                result.push(SpeakerSegment {
                    start_seconds: seg_start,
                    end_seconds: seg_end,
                    speaker_id: cur_speaker,
                });
                cur_speaker = *label;
                seg_start = chunk_start;
                seg_end = chunk_end;
            }
        }
        result.push(SpeakerSegment {
            start_seconds: seg_start,
            end_seconds: seg_end,
            speaker_id: cur_speaker,
        });

        let (result, id_map) = renumber_speakers(result);

        let centroids: HashMap<u32, Vec<f32>> = cluster_centroids
            .into_iter()
            .filter_map(|(old_id, c)| Some((*id_map.get(&old_id)?, c)))
            .collect();

        // Merge short-duration speakers into their nearest neighbour.
        let total_audio_secs = result.iter()
            .map(|s| s.end_seconds - s.start_seconds)
            .sum::<f64>();
        let (result, centroids) = merge_short_speakers(result, centroids, total_audio_secs);

        let n_spk: std::collections::HashSet<u32> = result.iter().map(|s| s.speaker_id).collect();
        log::warn!(
            "DIARIZATION: final result: {} speakers, {} segments",
            n_spk.len(),
            result.len(),
        );

        Ok(DiarizationOutput { segments: result, centroids })
    }
}

impl OrtDiarizationAdapter {
    fn extract_embedding(&self, audio: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
        self.extractor.extract_embedding(audio, sample_rate)
    }

    pub(crate) fn build_chunks(
        &self,
        samples: &[f32],
        sample_rate: u32,
        segments: &[(f64, f64)],
    ) -> Vec<Chunk> {
        let sr_f = sample_rate as f64;
        let t_chunk = std::time::Instant::now();
        let mut chunks: Vec<Chunk> = Vec::new();

        let speech_seconds: f64 = segments
            .iter()
            .map(|&(s, e)| (e - s).max(0.0))
            .filter(|&d| d >= MIN_SPEECH_SECS)
            .sum();
        let effective_split = (speech_seconds / MAX_DIARIZATION_CHUNKS as f64).max(SPLIT_TARGET_SECS);

        for &(start_s, end_s) in segments {
            let dur = end_s - start_s;
            if dur < MIN_SPEECH_SECS {
                continue;
            }

            if dur <= MAX_CHUNK_SECS {
                let start = ((start_s * sr_f) as usize).min(samples.len());
                let end = ((end_s * sr_f) as usize).min(samples.len());
                if end <= start {
                    continue;
                }
                let audio = &samples[start..end];
                if let Some(emb) = self.extract_embedding(audio, sample_rate) {
                    chunks.push(Chunk {
                        start_sample: start,
                        end_sample: end,
                        duration_secs: dur,
                        embedding: emb,
                    });
                }
            } else {
                // Split long segments into non-overlapping ~effective_split chunks
                let chunk_samples = (effective_split * sr_f) as usize;
                let start_base = (start_s * sr_f) as usize;
                let end_limit = ((end_s * sr_f) as usize).min(samples.len());
                let mut pos = start_base;
                while pos + chunk_samples <= end_limit {
                    let audio = &samples[pos..pos + chunk_samples];
                    let chunk_dur = chunk_samples as f64 / sr_f;
                    if let Some(emb) = self.extract_embedding(audio, sample_rate) {
                        chunks.push(Chunk {
                            start_sample: pos,
                            end_sample: pos + chunk_samples,
                            duration_secs: chunk_dur,
                            embedding: emb,
                        });
                    }
                    pos += chunk_samples;
                }
                // Handle remaining tail if it's long enough
                if pos < end_limit {
                    let tail_dur = (end_limit - pos) as f64 / sr_f;
                    if tail_dur >= MIN_SPEECH_SECS {
                        let audio = &samples[pos..end_limit];
                        if let Some(emb) = self.extract_embedding(audio, sample_rate) {
                            chunks.push(Chunk {
                                start_sample: pos,
                                end_sample: end_limit,
                                duration_secs: tail_dur,
                                embedding: emb,
                            });
                        }
                    }
                }
            }
        }

        log::warn!(
            "DIARIZATION: chunked + embedded {} chunks from {} segments in {:.2}s",
            chunks.len(),
            segments.len(),
            t_chunk.elapsed().as_secs_f64(),
        );

        chunks
    }

    // Pass-2 fine re-chunking (design D2): uniform non-overlapping windows at
    // `fine_split_secs` across the FULL recording, ignoring MAX_CHUNK_SECS so a
    // ≤10s segment is sub-divided rather than left as one mixed-speaker chunk.
    // Silent windows yield no embedding and are skipped. Embedding extraction is
    // parallelized with rayon — chunks are independent and the ONNX session is
    // thread-safe; collect() preserves temporal order so downstream assignment
    // and smoothing see the same sequence as the sequential path.
    pub(crate) fn build_fine_chunks(
        &self,
        samples: &[f32],
        sample_rate: u32,
        fine_split_secs: f64,
    ) -> Vec<Chunk> {
        let sr_f = sample_rate as f64;
        let ranges = fine_chunk_ranges(samples.len(), sample_rate, fine_split_secs);
        ranges
            .par_iter()
            .filter_map(|&(start, end)| {
                let audio = &samples[start..end];
                self.extract_embedding(audio, sample_rate).map(|emb| Chunk {
                    start_sample: start,
                    end_sample: end,
                    duration_secs: (end - start) as f64 / sr_f,
                    embedding: emb,
                })
            })
            .collect()
    }

    // Pass 2 (design D1): re-chunk the full recording at FINE_SPLIT_SECS and
    // assign each fine chunk to its nearest Pass-1 post-cap centroid, then
    // smooth, coalesce, and relabel temporal orphans (design D5). No second
    // AHC — every label is drawn from `final_centroids`, so the speaker cap
    // stays satisfied. Returns fine segments; the post-cap centroids stay
    // valid (commands.rs prunes any a speaker no longer uses).
    pub(crate) fn refine_pass2(
        &self,
        samples: &[f32],
        sample_rate: u32,
        final_centroids: &HashMap<u32, Vec<f32>>,
    ) -> Result<Vec<SpeakerSegment>> {
        if samples.is_empty() || final_centroids.is_empty() {
            return Ok(Vec::new());
        }
        let t = std::time::Instant::now();
        let chunks = self.build_fine_chunks(samples, sample_rate, FINE_SPLIT_SECS);
        let n_fine = chunks.len();
        if chunks.is_empty() {
            return Ok(Vec::new());
        }
        let embeddings: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
        let timestamps: Vec<f64> = chunks
            .iter()
            .map(|c| c.start_sample as f64 / sample_rate as f64)
            .collect();
        let durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();

        let labels = assign_to_nearest_centroids(&embeddings, &timestamps, final_centroids);
        let (labels, _) = smooth_to_fixed_point(
            &labels,
            &embeddings,
            &timestamps,
            &durations,
            final_centroids,
            &SmoothParams::default(),
        );
        let mut segments = chunks_to_segments(&chunks, &labels, sample_rate);
        relabel_temporal_orphans(&mut segments, MIN_PRESENCE_SECS, PRESENCE_WINDOW_SECS);
        let segments = merge_adjacent(segments);

        log::warn!(
            "DIARIZATION: Pass 2 refined {} fine chunks → {} segments in {:.2}s",
            n_fine,
            segments.len(),
            t.elapsed().as_secs_f64(),
        );
        Ok(segments)
    }
}

// Pure sample-range grid for fine re-chunking, separated from the
// model-dependent embedding step so the boundary logic is unit-testable.
// The final window is clipped to the buffer end: a tail shorter than
// `fine_split_secs` is emitted as-is (deterministic, no audio lost).
pub(crate) fn fine_chunk_ranges(
    total_samples: usize,
    sample_rate: u32,
    fine_split_secs: f64,
) -> Vec<(usize, usize)> {
    if total_samples == 0 {
        return Vec::new();
    }
    let sr_f = sample_rate as f64;
    let step = ((fine_split_secs * sr_f) as usize).max(1);
    let mut ranges = Vec::new();
    let mut pos = 0usize;
    while pos < total_samples {
        let end = (pos + step).min(total_samples);
        ranges.push((pos, end));
        pos = end;
    }
    ranges
}

// Top-2 cosine margin below which a fine chunk is considered near-equidistant
// between two centroids and falls back to its temporal predecessor (design D3).
// Tuned to the same scale as SMOOTH_CONFIDENCE_MARGIN (0.03): a value near 1e-4
// would only fire on exact floating-point ties, leaving D3 effectively dead.
const CENTROID_TIE_MARGIN: f32 = 0.02;

// Pass-2 nearest-centroid assignment (design D1). No second AHC: each fine
// chunk takes the label of its cosine-nearest Pass-1 post-cap centroid, so the
// output label set is a subset of the centroid keys by construction (cap
// invariant, task 3.4). A near-tie (top-2 within CENTROID_TIE_MARGIN) takes the
// temporal predecessor's label to avoid flicker at ambiguous boundaries (D3).
pub(crate) fn assign_to_nearest_centroids(
    embeddings: &[Vec<f32>],
    timestamps: &[f64],
    centroids: &HashMap<u32, Vec<f32>>,
) -> Vec<u32> {
    let n = embeddings.len();
    let default_label = *centroids.keys().min().unwrap_or(&0);
    let mut labels = vec![default_label; n];
    if n == 0 || centroids.is_empty() {
        return labels;
    }
    let order = temporal_order(timestamps);
    let mut prev_label: Option<u32> = None;
    for &i in &order {
        let emb = &embeddings[i];
        let mut scored: Vec<(u32, f32)> = centroids
            .iter()
            .filter(|(_, c)| {
                emb.iter().all(|x| x.is_finite()) && c.iter().all(|x| x.is_finite())
            })
            .map(|(&k, c)| (k, cosine_similarity(emb, c)))
            .collect();
        // Highest cosine, ties broken by smallest label (deterministic).
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.0.cmp(&b.0))
        });
        let label = match scored.first() {
            None => default_label,
            Some(&(best, best_s)) => {
                let second_s = scored.get(1).map(|t| t.1).unwrap_or(best_s);
                if best_s - second_s < CENTROID_TIE_MARGIN {
                    prev_label.unwrap_or(best)
                } else {
                    best
                }
            }
        };
        labels[i] = label;
        prev_label = Some(label);
    }
    labels
}

// Coalesce per-chunk labels into maximal same-label runs (pre-refactor twin of
// the inline step in process()). Used by refine_pass2 and its unit tests; the
// segments are pre-renumber and pre-merge_short_speakers, matching process()'s
// intermediate shape.
pub(crate) fn chunks_to_segments(
    chunks: &[Chunk],
    labels: &[u32],
    sample_rate: u32,
) -> Vec<SpeakerSegment> {
    let sr_f = sample_rate as f64;
    let mut indexed: Vec<(usize, u32)> = labels.iter().copied().enumerate().collect();
    indexed.sort_by_key(|(i, _)| chunks[*i].start_sample);
    let mut result: Vec<SpeakerSegment> = Vec::new();
    if indexed.is_empty() {
        return result;
    }
    let mut cur_speaker = indexed[0].1;
    let mut seg_start = chunks[indexed[0].0].start_sample as f64 / sr_f;
    let mut seg_end = chunks[indexed[0].0].end_sample as f64 / sr_f;
    for &(chunk_idx, label) in &indexed[1..] {
        let chunk = &chunks[chunk_idx];
        let chunk_start = chunk.start_sample as f64 / sr_f;
        let chunk_end = chunk.end_sample as f64 / sr_f;
        if label == cur_speaker {
            seg_end = chunk_end;
        } else {
            result.push(SpeakerSegment {
                start_seconds: seg_start,
                end_seconds: seg_end,
                speaker_id: cur_speaker,
            });
            cur_speaker = label;
            seg_start = chunk_start;
            seg_end = chunk_end;
        }
    }
    result.push(SpeakerSegment {
        start_seconds: seg_start,
        end_seconds: seg_end,
        speaker_id: cur_speaker,
    });
    result
}

/// Merge speakers whose total duration is below threshold into nearest neighbour.
/// Threshold = MIN_CLUSTER_FRAC × total audio, but never below MIN_SPEECH_SECS
/// (model can't produce embeddings from shorter clips anyway).
fn merge_short_speakers(
    mut segments: Vec<SpeakerSegment>,
    mut centroids: HashMap<u32, Vec<f32>>,
    total_audio_secs: f64,
) -> (Vec<SpeakerSegment>, HashMap<u32, Vec<f32>>) {
    let min_dur = (MIN_CLUSTER_FRAC * total_audio_secs).max(MIN_SPEECH_SECS);

    let mut speaker_dur: HashMap<u32, f64> = HashMap::new();
    for s in &segments {
        *speaker_dur.entry(s.speaker_id).or_default() += s.end_seconds - s.start_seconds;
    }

    let short_speakers: Vec<u32> = speaker_dur.iter()
        .filter(|(_, &dur)| dur < min_dur)
        .map(|(&id, _)| id)
        .collect();

    if short_speakers.is_empty() {
        return (segments, centroids);
    }

    let long_speakers: Vec<u32> = speaker_dur.iter()
        .filter(|(_, &dur)| dur >= min_dur)
        .map(|(&id, _)| id)
        .collect();

    if long_speakers.is_empty() {
        return (segments, centroids);
    }

    let mut remap: HashMap<u32, u32> = HashMap::new();
    for short_id in &short_speakers {
        let short_centroid = match centroids.get(short_id) {
            Some(c) => c,
            None => continue,
        };
        let best = long_speakers.iter()
            .filter_map(|&long_id| {
                centroids.get(&long_id).map(|c| (long_id, cosine_similarity(short_centroid, c)))
            })
            .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(id, _)| id);
        if let Some(target) = best {
            remap.insert(*short_id, target);
            log::debug!(
                "DIARIZATION: merging short speaker {} ({:.1}s) → speaker {}",
                short_id,
                speaker_dur[short_id],
                target,
            );
        }
    }

    // Remap segment speaker IDs
    for s in &mut segments {
        if let Some(&new_id) = remap.get(&s.speaker_id) {
            s.speaker_id = new_id;
        }
    }

    // Drop centroids for merged-away speakers
    for short_id in &short_speakers {
        if remap.contains_key(short_id) {
            centroids.remove(short_id);
        }
    }

    // Merge adjacent segments with same speaker and renumber.
    segments = merge_adjacent(segments);
    let (segments, renum_map) = renumber_speakers(segments);

    // Remap centroid keys to match final speaker IDs
    let centroids: HashMap<u32, Vec<f32>> = centroids
        .into_iter()
        .filter_map(|(old_id, c)| {
            let new_id = renum_map.get(&old_id).copied().unwrap_or(old_id);
            Some((new_id, c))
        })
        .collect();

    (segments, centroids)
}

fn merge_adjacent(mut segments: Vec<SpeakerSegment>) -> Vec<SpeakerSegment> {
    if segments.is_empty() {
        return segments;
    }
    segments.sort_by(|a, b| a.start_seconds.partial_cmp(&b.start_seconds).unwrap_or(std::cmp::Ordering::Equal));
    let mut merged = vec![segments[0].clone()];
    for s in segments.into_iter().skip(1) {
        let last = merged.last_mut().unwrap();
        if s.speaker_id == last.speaker_id && s.start_seconds <= last.end_seconds + 0.5 {
            last.end_seconds = last.end_seconds.max(s.end_seconds);
        } else {
            merged.push(s);
        }
    }
    merged
}

// Facet-2 temporal-presence floor (design D5). A fine segment at most one
// window long (≤ MIN_PRESENCE_SECS) whose speaker has NO other same-label
// segment within ±PRESENCE_WINDOW_SECS is an orphan — typically a
// vowel-dominated embedding globally nearest to a speaker who has not appeared
// (the [0:01] "Hello" → Ricardo case). It is relabeled to the dominant local
// speaker. A short segment that DOES have same-label support (a genuine
// interjection) is preserved. The window is symmetric and edge-clipped, so a
// first-turn chunk with only FUTURE support is kept (task 4.4). Fine segments
// are coalesced from FINE_SPLIT_SECS chunks, so the shortest is one window;
// ≤ MIN_PRESENCE_SECS therefore targets exactly the single-window segments.
pub(crate) const MIN_PRESENCE_SECS: f64 = FINE_SPLIT_SECS;
pub(crate) const PRESENCE_WINDOW_SECS: f64 = 30.0;

pub(crate) fn relabel_temporal_orphans(
    segments: &mut [SpeakerSegment],
    min_presence_secs: f64,
    window_secs: f64,
) {
    if segments.len() < 2 {
        return;
    }
    loop {
        let mut changed = false;
        for i in 0..segments.len() {
            let dur = segments[i].end_seconds - segments[i].start_seconds;
            if !(dur <= min_presence_secs) {
                continue;
            }
            let label = segments[i].speaker_id;
            let (lo, hi) = presence_window(&segments[i], window_secs);

            let has_support = segments.iter().enumerate().any(|(j, s)| {
                j != i && s.speaker_id == label && s.end_seconds > lo && s.start_seconds < hi
            });
            if has_support {
                continue;
            }

            // Orphan: relabel to the dominant OTHER speaker by overlap duration.
            let mut dur_by_label: std::collections::HashMap<u32, f64> =
                std::collections::HashMap::new();
            for (j, s) in segments.iter().enumerate() {
                if j == i || s.speaker_id == label {
                    continue;
                }
                let ov = (s.end_seconds.min(hi) - s.start_seconds.max(lo)).max(0.0);
                if ov > 0.0 {
                    *dur_by_label.entry(s.speaker_id).or_insert(0.0) += ov;
                }
            }
            // Max overlap duration; ties broken by smallest label (deterministic).
            let dominant = dur_by_label
                .iter()
                .max_by(|a, b| {
                    a.1.partial_cmp(b.1)
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then(b.0.cmp(a.0))
                })
                .map(|(&k, _)| k);
            if let Some(new_label) = dominant {
                if new_label != label {
                    segments[i].speaker_id = new_label;
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
}

fn presence_window(seg: &SpeakerSegment, window_secs: f64) -> (f64, f64) {
    let lo = (seg.start_seconds - window_secs).max(0.0);
    let hi = seg.end_seconds + window_secs;
    (lo, hi)
}

pub(crate) fn cluster_by_centroids(chunks: &[Chunk], threshold: f32) -> (Vec<u32>, HashMap<u32, Vec<f32>>) {
    let n = chunks.len();
    if n == 0 {
        return (Vec::new(), HashMap::new());
    }

    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut centroids: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
    let mut cluster_durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();
    let mut alive: Vec<bool> = vec![true; n];

    // Cached upper-triangle similarity matrix: sim[a] has n-a-1 entries,
    // sim[a][b-a-1] = cosine(centroids[a], centroids[b]) for a < b.
    // Computed once here; only row a + column a are recomputed on a merge into a
    // (all other pairs are unaffected). Scan uses O(1) lookups vs the naive's
    // O(d) cosine recompute — this is the ~200x speedup (design D1).
    let mut sim: Vec<Vec<f32>> = (0..n)
        .map(|a| {
            (a + 1..n)
                .map(|b| cosine_similarity(&centroids[a], &centroids[b]))
                .collect()
        })
        .collect();

    loop {
        let mut best_sim = threshold;
        let mut best_pair: Option<(usize, usize)> = None;

        for a in 0..n {
            if !alive[a] {
                continue;
            }
            for b in (a + 1)..n {
                if !alive[b] {
                    continue;
                }
                let s = sim[a][b - a - 1];
                if s > best_sim {
                    best_sim = s;
                    best_pair = Some((a, b));
                }
            }
        }

        let Some((a, b)) = best_pair else { break };

        let dur_a = cluster_durations[a];
        let dur_b = cluster_durations[b];
        let total_dur = dur_a + dur_b;
        let w_a = dur_a as f32 / total_dur as f32;
        let w_b = dur_b as f32 / total_dur as f32;

        let b_members = std::mem::take(&mut members[b]);
        let b_centroid = centroids[b].clone();
        for (i, v) in b_centroid.iter().enumerate() {
            centroids[a][i] = centroids[a][i] * w_a + v * w_b;
        }
        cluster_durations[a] = total_dur;
        members[a].extend(b_members);
        alive[b] = false;

        for x in (a + 1)..n {
            if alive[x] {
                sim[a][x - a - 1] = cosine_similarity(&centroids[a], &centroids[x]);
            }
        }
        for x in 0..a {
            if alive[x] {
                sim[x][a - x - 1] = cosine_similarity(&centroids[x], &centroids[a]);
            }
        }
    }

    let mut labels = vec![0u32; n];
    let mut next_label = 0u32;
    let mut label_map: HashMap<usize, u32> = HashMap::new();
    let mut final_centroids: HashMap<u32, Vec<f32>> = HashMap::new();
    for (idx, is_alive) in alive.iter().enumerate() {
        if !is_alive {
            continue;
        }
        let label = next_label;
        next_label += 1;
        label_map.insert(idx, label);
        for &member in &members[idx] {
            labels[member] = label;
        }
        final_centroids.insert(label, centroids[idx].clone());
    }

    (labels, final_centroids)
}

// ── Temporal-coherence smoothing (change: diarization-temporal-coherence) ──
//
// Global AHC labels each chunk against its cosine-nearest centroid with no
// memory of what its temporal neighbours received, so a contamination seed (a
// spurious cluster born before its speaker can be present) drifts and absorbs a
// real speaker mid-meeting. The pass below re-assigns each chunk by a ±W
// neighbourhood vote, gated so a clean meeting is a near-no-op, then collapses
// flicker islands. Centroids are recomputed from the cleaned labels so the
// embeddings stored for cross-meeting matching are not contaminated.

const SMOOTH_WINDOW: usize = 3;
const MIN_SMOOTH_SEGMENT_SECS: f64 = 10.0;
// Self (0.6) anchors a genuine interjection outright: it exceeds a single-side
// neighbour weight ~0.553 = exp(-1)+exp(-2)+exp(-3), so split neighbour votes
// cannot erase a real short turn — including an edge-of-array interjection that
// has neighbours on only one side. Yet 0.6 is low enough that a contaminated
// chunk's self-fit to its wrong centroid still loses to unanimous neighbours
// (absorption recovered). Margin 0.03 recovers drift up to cos~0.97, while a
// clean meeting's self-differential (~0.24 at between-speaker cos 0.6) is far
// above it, so clean input does not flip. neighbour decay is exp(-dist), peak
// exp(-1)≈0.368 at the nearest neighbour; self 0.6 is the strongest single vote
// but neighbours collectively (up to ~1.106 across both sides) outweigh it.
const SMOOTH_CONFIDENCE_MARGIN: f32 = 0.03;
const SMOOTH_MAX_ITERS: usize = 2;
const SMOOTH_SELF_WEIGHT: f32 = 0.6;

pub(crate) struct SmoothParams {
    pub(crate) window: usize,
    pub(crate) min_segment_secs: f64,
    pub(crate) confidence_margin: f32,
    pub(crate) self_weight: f32,
    pub(crate) max_iters: usize,
}

impl Default for SmoothParams {
    fn default() -> Self {
        SmoothParams {
            window: SMOOTH_WINDOW,
            min_segment_secs: MIN_SMOOTH_SEGMENT_SECS,
            confidence_margin: SMOOTH_CONFIDENCE_MARGIN,
            self_weight: SMOOTH_SELF_WEIGHT,
            max_iters: SMOOTH_MAX_ITERS,
        }
    }
}

// Re-assign each chunk by a ±W neighbourhood vote (design D2). The chunk's own
// embedding votes too, but DAMPED below the neighbour weight: a contaminated
// chunk's self-fit to its (wrong) centroid is low, so unanimous neighbours still
// outvote it and pull it back (absorption recovery); a genuine short interjection
// votes strongly for its own distinct centroid, so the split neighbour votes on
// either side cannot beat it by the confidence margin and it is preserved rather
// than merged away. A flip happens only when the winner's normalised score beats
// the current label's by `confidence_margin` (design D2.3), so a clean meeting is
// a near-no-op. A minimum-duration floor then collapses flicker islands (design D3).
pub(crate) fn smooth_labels_temporal(
    labels: &[u32],
    embeddings: &[Vec<f32>],
    timestamps: &[f64],
    durations: &[f64],
    centroids: &HashMap<u32, Vec<f32>>,
    params: &SmoothParams,
) -> Vec<u32> {
    if labels.is_empty() {
        return Vec::new();
    }
    let order = temporal_order(timestamps);
    let mut out = labels.to_vec();
    let w = params.window;

    for pos in 0..order.len() {
        let i = order[pos];
        let lo = pos.saturating_sub(w);
        let hi = (pos + w + 1).min(order.len());
        let mut total_w: f32 = 0.0;
        let mut score_sum: HashMap<u32, f32> = HashMap::new();
        for q in lo..hi {
            let j = order[q];
            if !embeddings[j].iter().all(|x| x.is_finite()) {
                continue; // degenerate embedding contributes 0 (design D2 clamp)
            }
            let dist = (q as i32 - pos as i32).unsigned_abs() as f32;
            let dec = if q == pos { params.self_weight } else { (-dist).exp() };
            total_w += dec;
            for (&k, cent) in centroids {
                let c = cosine_similarity(&embeddings[j], cent);
                *score_sum.entry(k).or_insert(0.0) += if c.is_finite() { c * dec } else { 0.0 };
            }
        }
        if total_w <= 0.0 {
            continue; // no finite-neighbour signal; keep current label
        }
        let cur = labels[i];
        let cur_score = score_sum.get(&cur).copied().unwrap_or(0.0) / total_w;
        // Deterministic winner: highest score, ties broken by smallest label, so
        // output is independent of HashMap iteration order (required by the spec).
        let mut winner: Option<(u32, f32)> = None;
        for (&k, &s) in &score_sum {
            match winner {
                None => winner = Some((k, s)),
                Some((bk, bs)) => {
                    let better = s > bs || (s == bs && k < bk);
                    if better {
                        winner = Some((k, s));
                    }
                }
            }
        }
        if let Some((wk, ws)) = winner {
            let win_score = ws / total_w;
            if wk != cur && win_score > cur_score + params.confidence_margin {
                out[i] = wk;
            }
        }
    }

    enforce_min_segment_floor(&mut out, &order, durations, params);
    out
}

// Duration-weighted centroid per label from cleaned labels (design D4). A cluster
// with no surviving chunks has zero duration and is dropped, not preserved as a
// phantom centroid that would pollute the cross-meeting registry.
pub(crate) fn recompute_centroids_from_labels(
    labels: &[u32],
    embeddings: &[Vec<f32>],
    durations: &[f64],
) -> HashMap<u32, Vec<f32>> {
    let dim = embeddings.first().map(|v| v.len()).unwrap_or(0);
    let mut sum: HashMap<u32, Vec<f64>> = HashMap::new();
    let mut total: HashMap<u32, f64> = HashMap::new();
    for i in 0..labels.len() {
        let l = labels[i];
        let acc = sum.entry(l).or_insert(vec![0.0f64; dim]);
        for (k, &v) in embeddings[i].iter().enumerate() {
            if v.is_finite() {
                acc[k] += v as f64 * durations[i];
            }
        }
        *total.entry(l).or_insert(0.0) += durations[i];
    }
    sum.into_iter()
        .filter_map(|(l, acc)| {
            let td = total.get(&l).copied().filter(|&d| d > 0.0)?;
            Some((l, acc.iter().map(|&x| (x / td) as f32).collect()))
        })
        .collect()
}

// Iterate vote → recompute centroids to a fixed point, capped at max_iters
// (design D7: 2 by default). The first pass de-contaminates the clearest chunks
// using the (still-drifted) clustering centroids; the recomputed centroids are
// cleaner, so the second pass recovers the rest. Always returns centroids that
// match the returned labels.
pub(crate) fn smooth_to_fixed_point(
    labels: &[u32],
    embeddings: &[Vec<f32>],
    timestamps: &[f64],
    durations: &[f64],
    centroids: &HashMap<u32, Vec<f32>>,
    params: &SmoothParams,
) -> (Vec<u32>, HashMap<u32, Vec<f32>>) {
    let mut labels = labels.to_vec();
    let mut centroids = centroids.clone();
    for _ in 0..params.max_iters {
        let next =
            smooth_labels_temporal(&labels, embeddings, timestamps, durations, &centroids, params);
        if next == labels {
            break;
        }
        labels = next;
        centroids = recompute_centroids_from_labels(&labels, embeddings, durations);
    }
    (labels, centroids)
}

fn temporal_order(timestamps: &[f64]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..timestamps.len())
        .filter(|&i| timestamps[i].is_finite())
        .collect();
    order.sort_by(|&a, &b| {
        timestamps[a].partial_cmp(&timestamps[b]).unwrap_or(std::cmp::Ordering::Equal)
    });
    order
}

// Collapse flicker islands: a same-label run shorter than min_segment_secs whose
// two adjacent runs share one (different) label is a noise spike inside that
// speaker's region and is absorbed. A short run sandwiched between two DIFFERENT
// speakers is a genuine interjection and is preserved — over-merging destroys
// real turns, while a leftover sub-2% segment is reclaimed harmlessly downstream
// by merge_short_speakers.
fn enforce_min_segment_floor(
    labels: &mut [u32],
    order: &[usize],
    durations: &[f64],
    params: &SmoothParams,
) {
    if order.is_empty() {
        return;
    }
    loop {
        let runs = runs_along_order(labels, order);
        let mut merged = false;
        for ri in 0..runs.len() {
            let (rs, re, rl) = runs[ri];
            let dur: f64 = (rs..=re).map(|p| durations[order[p]]).sum();
            if dur >= params.min_segment_secs {
                continue;
            }
            let left = ri.checked_sub(1).map(|k| runs[k].2);
            let right = runs.get(ri + 1).map(|t| t.2);
            if let (Some(l), Some(r)) = (left, right) {
                if l == r && l != rl {
                    for p in rs..=re {
                        labels[order[p]] = l;
                    }
                    merged = true;
                    break;
                }
            }
        }
        if !merged {
            break;
        }
    }
}

fn runs_along_order(labels: &[u32], order: &[usize]) -> Vec<(usize, usize, u32)> {
    let mut runs: Vec<(usize, usize, u32)> = Vec::new();
    let mut start = 0usize;
    for pos in 1..=order.len() {
        let boundary = pos == order.len() || labels[order[pos]] != labels[order[start]];
        if boundary {
            runs.push((start, pos - 1, labels[order[start]]));
            start = pos;
        }
    }
    runs
}

#[cfg(test)]
pub(crate) fn cluster_by_centroids_naive(chunks: &[Chunk], threshold: f32) -> (Vec<u32>, HashMap<u32, Vec<f32>>) {
    let n = chunks.len();
    if n == 0 {
        return (Vec::new(), HashMap::new());
    }

    let mut members: Vec<Vec<usize>> = (0..n).map(|i| vec![i]).collect();
    let mut centroids: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
    let mut cluster_durations: Vec<f64> = chunks.iter().map(|c| c.duration_secs).collect();
    let mut alive: Vec<bool> = vec![true; n];

    loop {
        let mut best_sim = threshold;
        let mut best_pair: Option<(usize, usize)> = None;

        let alive_indices: Vec<usize> = alive.iter().enumerate()
            .filter(|(_, &a)| a)
            .map(|(i, _)| i)
            .collect();

        for i_idx in 0..alive_indices.len() {
            for j_idx in (i_idx + 1)..alive_indices.len() {
                let a = alive_indices[i_idx];
                let b = alive_indices[j_idx];
                let sim = cosine_similarity(&centroids[a], &centroids[b]);
                if sim > best_sim {
                    best_sim = sim;
                    best_pair = Some((a, b));
                }
            }
        }

        let Some((a, b)) = best_pair else { break };

        let dur_a = cluster_durations[a];
        let dur_b = cluster_durations[b];
        let total_dur = dur_a + dur_b;
        let w_a = dur_a as f32 / total_dur as f32;
        let w_b = dur_b as f32 / total_dur as f32;

        let b_members = std::mem::take(&mut members[b]);
        let b_centroid = centroids[b].clone();
        for (i, v) in b_centroid.iter().enumerate() {
            centroids[a][i] = centroids[a][i] * w_a + v * w_b;
        }
        cluster_durations[a] = total_dur;
        members[a].extend(b_members);
        alive[b] = false;
    }

    let mut labels = vec![0u32; n];
    let mut next_label = 0u32;
    let mut label_map: HashMap<usize, u32> = HashMap::new();
    let mut final_centroids: HashMap<u32, Vec<f32>> = HashMap::new();
    for (idx, is_alive) in alive.iter().enumerate() {
        if !is_alive {
            continue;
        }
        let label = next_label;
        next_label += 1;
        label_map.insert(idx, label);
        for &member in &members[idx] {
            labels[member] = label;
        }
        final_centroids.insert(label, centroids[idx].clone());
    }

    (labels, final_centroids)
}

pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a < 1e-8 || norm_b < 1e-8 {
        return 0.0;
    }
    dot / (norm_a * norm_b)
}

// The diarization pipeline strips silence earlier via energy VAD, but the
// extractor guards again: a direct caller must not feed the model silence and
// get back a degenerate vector that corrupts centroid clustering. Threshold is
// mean-square energy; RMS < 1e-5 is ~-100 dBFS, far below any real speech.
fn is_effectively_silent(audio: &[f32]) -> bool {
    if audio.is_empty() {
        return true;
    }
    let sum_sq: f32 = audio.iter().map(|&s| s * s).sum();
    (sum_sq / audio.len() as f32) < 1e-10
}

fn renumber_speakers(segments: Vec<SpeakerSegment>) -> (Vec<SpeakerSegment>, HashMap<u32, u32>) {
    let mut mapping: HashMap<u32, u32> = HashMap::new();
    let mut next_id: u32 = 0;
    let result = segments
        .into_iter()
        .map(|mut s| {
            let assigned = *mapping.entry(s.speaker_id).or_insert_with(|| {
                let id = next_id;
                next_id += 1;
                id
            });
            s.speaker_id = assigned;
            s
        })
        .collect();
    (result, mapping)
}

fn energy_vad_segments(samples: &[f32], sample_rate: u32) -> Vec<(f64, f64)> {
    let sr = sample_rate as usize;
    let window_ms = 30;
    let window_samples = (sr * window_ms / 1000).max(1);

    let mut rms_values: Vec<f32> = samples
        .chunks(window_samples)
        .map(|c| {
            let sum: f32 = c.iter().map(|s| s * s).sum();
            (sum / c.len() as f32).sqrt()
        })
        .collect();
    rms_values.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let median = rms_values[rms_values.len() / 2];
    let threshold = (median * 0.1).max(0.001);

    let mut speech: Vec<(usize, usize)> = Vec::new();
    let mut in_speech = false;
    let mut seg_start = 0usize;
    let mut pos = 0;
    while pos + window_samples <= samples.len() {
        let chunk = &samples[pos..pos + window_samples];
        let rms = (chunk.iter().map(|s| s * s).sum::<f32>() / chunk.len() as f32).sqrt();
        if rms > threshold {
            if !in_speech {
                seg_start = pos;
                in_speech = true;
            }
        } else if in_speech {
            speech.push((seg_start, pos));
            in_speech = false;
        }
        pos += window_samples;
    }
    if in_speech {
        speech.push((seg_start, samples.len()));
    }

    // Merge nearby segments (< 300ms gap)
    let min_gap = (0.3 * sr as f64) as usize;
    let merged = merge_nearby_segments(speech, min_gap);

    merged
        .into_iter()
        .map(|(s, e)| (s as f64 / sr as f64, e as f64 / sr as f64))
        .collect()
}

fn merge_nearby_segments(segments: Vec<(usize, usize)>, min_gap: usize) -> Vec<(usize, usize)> {
    if segments.is_empty() {
        return segments;
    }
    let mut merged = Vec::with_capacity(segments.len());
    let mut cur = segments[0];
    for seg in &segments[1..] {
        if seg.0.saturating_sub(cur.1) < min_gap {
            cur.1 = cur.1.max(seg.1);
        } else {
            merged.push(cur);
            cur = *seg;
        }
    }
    merged.push(cur);
    merged
}

/// Pure-Rust in-memory speaker registry (replaces sherpa's
/// `SpeakerEmbeddingManager`). Same on-disk schema (`speaker_embeddings`
/// table, nemo_titanet 192-dim vectors) — only the backing store changed.
///
/// `search` parity (load-bearing, design D3 + task 4.1): sherpa's manager
/// scans EVERY stored vector and returns the name of the single
/// highest-cosine vector ≥ threshold — a per-vector best-score scan, NOT a
/// per-speaker centroid. This port matches that exactly.
pub struct CosineRegistryAdapter {
    /// name → all enrolled embedding vectors (multiple enrollments per
    /// speaker). Mutex for interior mutability behind the trait's `&self`.
    speakers: std::sync::Mutex<HashMap<String, Vec<Vec<f32>>>>,
    dim: usize,
}

impl CosineRegistryAdapter {
    pub fn new(dim: usize) -> Result<Self> {
        if dim == 0 {
            return Err(anyhow!("registry dimension must be non-zero"));
        }
        Ok(Self {
            speakers: std::sync::Mutex::new(HashMap::new()),
            dim,
        })
    }
}

impl SpeakerIdentificationPort for CosineRegistryAdapter {
    fn add(&self, name: &str, embedding: &EmbeddingVector) -> Result<()> {
        if embedding.dim() != self.dim {
            return Err(anyhow!(
                "embedding dimension mismatch: expected {}, got {}",
                self.dim,
                embedding.dim()
            ));
        }
        let mut guard = self
            .speakers
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        guard
            .entry(name.to_string())
            .or_default()
            .push(embedding.as_slice().to_vec());
        Ok(())
    }

    fn add_list(&self, name: &str, embeddings: &[EmbeddingVector]) -> Result<()> {
        for e in embeddings {
            if e.dim() != self.dim {
                return Err(anyhow!(
                    "embedding dimension mismatch: expected {}, got {}",
                    self.dim,
                    e.dim()
                ));
            }
        }
        let mut guard = self
            .speakers
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let entry = guard.entry(name.to_string()).or_default();
        for e in embeddings {
            entry.push(e.as_slice().to_vec());
        }
        Ok(())
    }

    fn search(&self, embedding: &EmbeddingVector, threshold: f32) -> Result<Option<String>> {
        if embedding.dim() != self.dim {
            return Err(anyhow!(
                "embedding dimension mismatch: expected {}, got {}",
                self.dim,
                embedding.dim()
            ));
        }
        let guard = self
            .speakers
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        // Per-vector best-score scan (sherpa parity): iterate every stored
        // vector across all names; the name owning the single highest-cosine
        // vector ≥ threshold wins.
        let mut best: Option<(String, f32)> = None;
        for (name, vecs) in guard.iter() {
            for v in vecs {
                let score = cosine_similarity(embedding.as_slice(), v);
                if score >= threshold {
                    match &best {
                        Some((_, s)) if *s >= score => {}
                        _ => best = Some((name.clone(), score)),
                    }
                }
            }
        }
        Ok(best.map(|(name, _)| name))
    }

    fn verify(&self, name: &str, embedding: &EmbeddingVector, threshold: f32) -> Result<bool> {
        if embedding.dim() != self.dim {
            return Err(anyhow!(
                "embedding dimension mismatch: expected {}, got {}",
                self.dim,
                embedding.dim()
            ));
        }
        let guard = self
            .speakers
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        let Some(vecs) = guard.get(name) else {
            return Ok(false);
        };
        // sherpa's verify: any stored vector for `name` ≥ threshold.
        Ok(vecs
            .iter()
            .any(|v| cosine_similarity(embedding.as_slice(), v) >= threshold))
    }

    fn remove(&self, name: &str) -> Result<()> {
        let mut guard = self
            .speakers
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        if guard.remove(name).is_none() {
            return Err(anyhow!("failed to remove speaker: {}", name));
        }
        Ok(())
    }

    fn list_speakers(&self) -> Result<Vec<String>> {
        let guard = self
            .speakers
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        Ok(guard.keys().cloned().collect())
    }

    fn contains(&self, name: &str) -> Result<bool> {
        let guard = self
            .speakers
            .lock()
            .map_err(|_| anyhow!("registry lock poisoned"))?;
        Ok(guard.contains_key(name))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedding_adapter_rejects_nonexistent_model_path() {
        match super::super::nemo_extractor::NemoEmbeddingExtractor::new("/nonexistent/model.onnx") {
            Err(e) => assert!(e.to_string().contains("embedding model not found"), "unexpected error: {}", e),
            Ok(_) => panic!("expected error for nonexistent model path"),
        }
    }

    #[test]
    fn diarization_adapter_rejects_nonexistent_embedding_model() {
        match OrtDiarizationAdapter::new("/nonexistent/embedding.onnx", "/nonexistent/segmentation.onnx") {
            Err(e) => assert!(e.to_string().contains("diarization model not found"), "unexpected error: {}", e),
            Ok(_) => panic!("expected error for nonexistent model path"),
        }
    }

    #[test]
    fn diarization_adapter_rejects_nonexistent_segmentation_model() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let path = tmp.path().to_str().unwrap();
        match OrtDiarizationAdapter::new(path, "/nonexistent/segmentation.onnx") {
            Err(e) => assert!(e.to_string().contains("segmentation model not found"), "unexpected error: {}", e),
            Ok(_) => panic!("expected error for nonexistent model path"),
        }
    }

    #[test]
    fn registry_search_empty_returns_none() {
        let registry = CosineRegistryAdapter::new(256).unwrap();
        let embedding = EmbeddingVector::from_slice(&vec![0.1f32; 256], 256).unwrap();
        let result = registry.search(&embedding, 0.5).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn registry_search_below_threshold_returns_none() {
        let registry = CosineRegistryAdapter::new(4).unwrap();
        let alice_embedding = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0], 4).unwrap();
        registry.add("Alice", &alice_embedding).unwrap();

        let query = EmbeddingVector::from_slice(&vec![0.0, 1.0, 0.0, 0.0], 4).unwrap();
        let result = registry.search(&query, 0.99).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn registry_add_and_search_match() {
        let registry = CosineRegistryAdapter::new(4).unwrap();
        let alice = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0], 4).unwrap();
        registry.add("Alice", &alice).unwrap();

        let query = EmbeddingVector::from_slice(&vec![0.9, 0.1, 0.0, 0.0], 4).unwrap();
        let result = registry.search(&query, 0.5).unwrap();
        assert_eq!(result.as_deref(), Some("Alice"));
    }

    #[test]
    fn registry_dimension_mismatch_rejected() {
        let registry = CosineRegistryAdapter::new(4).unwrap();
        let wrong_dim = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0, 0.0], 5).unwrap();
        let result = registry.add("Alice", &wrong_dim);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("dimension mismatch"));
    }

    #[test]
    fn registry_search_dimension_mismatch_rejected() {
        let registry = CosineRegistryAdapter::new(4).unwrap();
        let wrong_dim = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0, 0.0], 5).unwrap();
        let result = registry.search(&wrong_dim, 0.5);
        assert!(result.is_err());
    }

    #[test]
    fn registry_list_speakers_empty() {
        let registry = CosineRegistryAdapter::new(4).unwrap();
        assert!(registry.list_speakers().unwrap().is_empty());
    }

    #[test]
    fn registry_remove_nonexistent_fails() {
        let registry = CosineRegistryAdapter::new(4).unwrap();
        assert!(registry.remove("ghost").is_err());
    }

    #[test]
    fn registry_add_remove_lifecycle() {
        let registry = CosineRegistryAdapter::new(4).unwrap();
        let emb = EmbeddingVector::from_slice(&vec![1.0, 0.0, 0.0, 0.0], 4).unwrap();
        registry.add("Alice", &emb).unwrap();
        assert!(registry.contains("Alice").unwrap());
        registry.remove("Alice").unwrap();
        assert!(!registry.contains("Alice").unwrap());
    }

    #[test]
    fn cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &a) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) + 1.0).abs() < 1e-6);
    }

    #[test]
    fn cosine_similarity_zero_vector() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 0.0];
        assert!((cosine_similarity(&a, &b)).abs() < 1e-6);
    }

    #[test]
    fn is_effectively_silent_rejects_all_zeros() {
        assert!(is_effectively_silent(&vec![0.0f32; 16000]));
    }

    #[test]
    fn is_effectively_silent_rejects_empty() {
        assert!(is_effectively_silent(&[]));
    }

    #[test]
    fn is_effectively_silent_accepts_real_audio() {
        let audio: Vec<f32> = (0..16000).map(|i| ((i as f32) * 0.001).sin() * 0.1).collect();
        assert!(!is_effectively_silent(&audio));
    }

    #[test]
    fn cluster_single_chunk_gets_one_label() {
        let chunks = vec![Chunk {
            start_sample: 0,
            end_sample: 48000,
            duration_secs: 3.0,
            embedding: vec![1.0; 8],
        }];
        let (labels, centroids) = cluster_by_centroids(&chunks, 0.5);
        assert_eq!(labels.len(), 1);
        assert_eq!(labels[0], 0);
        assert_eq!(centroids.len(), 1);
    }

    #[test]
    fn cluster_two_identical_chunks_merge() {
        let chunks = vec![
            Chunk { start_sample: 0, end_sample: 48000, duration_secs: 3.0, embedding: vec![1.0, 0.0, 0.0, 0.0] },
            Chunk { start_sample: 48000, end_sample: 96000, duration_secs: 3.0, embedding: vec![1.0, 0.0, 0.0, 0.0] },
        ];
        let (labels, _centroids) = cluster_by_centroids(&chunks, 0.5);
        assert_eq!(labels.len(), 2);
        assert_eq!(labels[0], labels[1], "identical embeddings should merge");
    }

    #[test]
    fn cluster_orthogonal_chunks_stay_separate() {
        let chunks = vec![
            Chunk { start_sample: 0, end_sample: 48000, duration_secs: 3.0, embedding: vec![1.0, 0.0, 0.0, 0.0] },
            Chunk { start_sample: 48000, end_sample: 96000, duration_secs: 3.0, embedding: vec![0.0, 1.0, 0.0, 0.0] },
        ];
        let (labels, centroids) = cluster_by_centroids(&chunks, 0.99);
        assert_eq!(labels.len(), 2);
        assert_ne!(labels[0], labels[1], "orthogonal embeddings should stay separate at high threshold");
        assert_eq!(centroids.len(), 2);
    }

    #[test]
    fn renumber_speakers_contiguous() {
        let segments = vec![
            SpeakerSegment { start_seconds: 0.0, end_seconds: 1.0, speaker_id: 42 },
            SpeakerSegment { start_seconds: 1.0, end_seconds: 2.0, speaker_id: 7 },
            SpeakerSegment { start_seconds: 2.0, end_seconds: 3.0, speaker_id: 42 },
        ];
        let (renumbered, _mapping) = renumber_speakers(segments);
        assert_eq!(renumbered[0].speaker_id, 0);
        assert_eq!(renumbered[1].speaker_id, 1);
        assert_eq!(renumbered[2].speaker_id, 0);
    }

    #[test]
    fn fp_roundtrip() {
        let v = 0.65f32;
        assert!((from_fp(to_fp(v)) - v).abs() < 0.001);
    }

    // ── diarization-label-quality — fine re-chunk boundary tests (§3.2/3.5/3.7)

    #[test]
    fn fine_chunk_ranges_empty() {
        assert!(fine_chunk_ranges(0, 16000, FINE_SPLIT_SECS).is_empty());
    }

    #[test]
    fn fine_chunk_ranges_exact_multiple() {
        // 4s at 16kHz, 2s split → two full 2s windows, no tail.
        let r = fine_chunk_ranges(4 * 16000, 16000, FINE_SPLIT_SECS);
        assert_eq!(r, vec![(0, 32000), (32000, 64000)]);
        let total: usize = r.iter().map(|(s, e)| e - s).sum();
        assert_eq!(total, 4 * 16000);
    }

    #[test]
    fn fine_chunk_ranges_tail_is_kept_and_clipped() {
        // 5s at 16kHz, 2s split → two full + one 1s tail (kept, not dropped).
        let r = fine_chunk_ranges(5 * 16000, 16000, FINE_SPLIT_SECS);
        assert_eq!(r.len(), 3);
        assert_eq!(r[2], (64000, 80000));
        assert_eq!(r[2].1 - r[2].0, 16000); // 1s tail
        let total: usize = r.iter().map(|(s, e)| e - s).sum();
        assert_eq!(total, 5 * 16000);
    }

    #[test]
    fn fine_chunk_ranges_non_overlapping_contiguous() {
        let r = fine_chunk_ranges(7 * 16000, 16000, FINE_SPLIT_SECS);
        for w in r.windows(2) {
            assert_eq!(w[0].1, w[1].0, "windows must be contiguous, not overlapping");
        }
        assert_eq!(r[0].0, 0);
        assert_eq!(r.last().unwrap().1, 7 * 16000);
    }

    #[test]
    fn fine_chunk_ranges_step_floor_prevents_infinite_loop() {
        // split×sr truncates to 0 (0.5s @ 1 Hz): step clamps to ≥1, no infinite loop.
        let r = fine_chunk_ranges(5, 1, 0.5);
        assert_eq!(r, vec![(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)]);
    }

    #[test]
    fn fine_chunk_ranges_oversized_is_bounded() {
        // 4h @ 16kHz ≈ 230M samples; grid must build without OOM/panic.
        let total = (4 * 3600) * 16000;
        let r = fine_chunk_ranges(total, 16000, FINE_SPLIT_SECS);
        let expected = total.div_ceil(2 * 16000);
        assert_eq!(r.len(), expected);
        assert_eq!(r.last().unwrap().1, total);
    }

    // ── diarization-label-quality — Pass-2 assignment tests (§3.1/3.4/3.6)

    fn fine_chunk_at(t_secs: f64, sr: u32, dur_secs: f64, emb: Vec<f32>) -> Chunk {
        let s = (t_secs * sr as f64) as usize;
        Chunk {
            start_sample: s,
            end_sample: s + (dur_secs * sr as f64) as usize,
            duration_secs: dur_secs,
            embedding: emb,
        }
    }

    #[test]
    fn assign_interjection_chunk_takes_second_speaker() {
        // 5 fine chunks; chunk 2 (t=4s) is the interjection nearest centroid B.
        let sr = 16000u32;
        let a = vec![1.0f32, 0.0, 0.0, 0.0];
        let b = vec![0.0f32, 1.0, 0.0, 0.0];
        let chunks = vec![
            fine_chunk_at(0.0, sr, 2.0, a.clone()),
            fine_chunk_at(2.0, sr, 2.0, a.clone()),
            fine_chunk_at(4.0, sr, 2.0, b.clone()),
            fine_chunk_at(6.0, sr, 2.0, a.clone()),
            fine_chunk_at(8.0, sr, 2.0, a.clone()),
        ];
        let embeddings: Vec<Vec<f32>> = chunks.iter().map(|c| c.embedding.clone()).collect();
        let timestamps: Vec<f64> = chunks.iter().map(|c| c.start_sample as f64 / sr as f64).collect();
        let centroids: HashMap<u32, Vec<f32>> = [(10u32, a), (20u32, b)].into_iter().collect();

        let labels = assign_to_nearest_centroids(&embeddings, &timestamps, &centroids);
        assert_eq!(labels[2], 20, "interjection chunk takes the second speaker");

        let segs = chunks_to_segments(&chunks, &labels, sr);
        // Three runs: A | B (interjection) | A — the coarse run is split.
        assert_eq!(segs.len(), 3, "coarse run split into 3 by the interjection");
        assert_eq!(segs[1].speaker_id, 20);
        let interjection = &segs[1];
        assert!((interjection.start_seconds - 4.0).abs() < 1e-6);
        assert!((interjection.end_seconds - 6.0).abs() < 1e-6);
    }

    #[test]
    fn assign_labels_are_subset_of_centroid_keys() {
        let cents: HashMap<u32, Vec<f32>> = [
            (1u32, vec![1.0, 0.0, 0.0]),
            (2u32, vec![0.0, 1.0, 0.0]),
            (3u32, vec![0.0, 0.0, 1.0]),
        ]
        .into_iter()
        .collect();
        let embeddings = vec![
            vec![0.9f32, 0.1, 0.0],
            vec![0.1f32, 0.9, 0.0],
            vec![0.0f32, 0.0, 1.0],
            vec![0.4f32, 0.4, 0.2],
        ];
        let timestamps = vec![0.0, 2.0, 4.0, 6.0];
        let labels = assign_to_nearest_centroids(&embeddings, &timestamps, &cents);
        for &l in &labels {
            assert!(cents.contains_key(&l), "Pass 2 invented label {}", l);
        }
        assert_eq!(labels.len(), embeddings.len());
    }

    #[test]
    fn assign_single_centroid_no_fragmentation() {
        let only = vec![1.0f32, 0.0, 0.0];
        let centroids: HashMap<u32, Vec<f32>> = [(7u32, only.clone())].into_iter().collect();
        let embeddings = vec![
            vec![0.8f32, 0.2, 0.0],
            vec![0.6f32, 0.4, 0.1],
            vec![0.5f32, 0.3, 0.2],
        ];
        let timestamps = vec![0.0, 2.0, 4.0];
        let labels = assign_to_nearest_centroids(&embeddings, &timestamps, &centroids);
        assert!(labels.iter().all(|&l| l == 7), "single-centroid meeting not fragmented");
    }

    #[test]
    fn assign_equidistant_chunk_takes_predecessor() {
        // chunk0 clearly A; chunk1 equidistant (cos equal) → predecessor A (D3).
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        let equi = vec![1.0f32 / 2f32.sqrt(), 1.0 / 2f32.sqrt()]; // cos=a cos=b
        let embeddings = vec![a.clone(), equi];
        let timestamps = vec![0.0, 2.0];
        let centroids: HashMap<u32, Vec<f32>> = [(10u32, a), (20u32, b)].into_iter().collect();
        let labels = assign_to_nearest_centroids(&embeddings, &timestamps, &centroids);
        assert_eq!(labels[0], 10);
        assert_eq!(labels[1], 10, "equidistant chunk takes predecessor, not arbitrary");
    }

    // ── diarization-label-quality — §4 adversarial: degenerate Pass-2 inputs

    #[test]
    fn assign_nan_embedding_falls_back_to_default_label() {
        // A NaN embedding makes cosine undefined against every centroid; the
        // finite-filter drops all candidates and the chunk takes the default
        // label (smallest centroid key) rather than panicking.
        let dim = 4;
        let a = emb(0, dim);
        let b = emb(1, dim);
        let nan = vec![f32::NAN; dim];
        let embeddings = vec![a.clone(), nan, b.clone()];
        let timestamps = vec![0.0, 2.0, 4.0];
        let centroids: HashMap<u32, Vec<f32>> = [(10u32, a), (20u32, b)].into_iter().collect();
        let labels = assign_to_nearest_centroids(&embeddings, &timestamps, &centroids);
        assert_eq!(labels.len(), 3);
        assert_eq!(labels[0], 10);
        assert_eq!(labels[1], 10, "NaN embedding takes default label, no panic");
        assert_eq!(labels[2], 20);
    }

    #[test]
    fn assign_and_coalesce_empty_input_produces_no_phantom_speakers() {
        // All-silent audio yields zero fine chunks; Pass-2's tail helpers must
        // return empty (not invent a speaker) and never panic on empty input.
        let centroids: HashMap<u32, Vec<f32>> =
            [(10u32, emb(0, 4)), (20u32, emb(1, 4))].into_iter().collect();
        let labels = assign_to_nearest_centroids(&[], &[], &centroids);
        assert!(labels.is_empty());

        let empty_centroids: HashMap<u32, Vec<f32>> = HashMap::new();
        let labels2 = assign_to_nearest_centroids(&[emb(0, 4)], &[0.0], &empty_centroids);
        assert_eq!(labels2, vec![0], "no centroids → default label 0, no panic");

        let segs = chunks_to_segments(&[], &[], 16000);
        assert!(segs.is_empty(), "empty chunks → no phantom segments");
    }

    // ── diarization-label-quality — facet-2 temporal-presence tests (§4.1/4.3/4.4)

    fn seg(start: f64, end: f64, speaker: u32) -> SpeakerSegment {
        SpeakerSegment {
            start_seconds: start,
            end_seconds: end,
            speaker_id: speaker,
        }
    }

    #[test]
    fn orphan_without_nearby_support_is_relabeled() {
        // [0:01] "Hello" analog: a single-window RIC segment before RIC joins.
        let mut segments = vec![seg(0.0, 2.0, 30), seg(2.0, 30.0, 10)];
        relabel_temporal_orphans(&mut segments, MIN_PRESENCE_SECS, PRESENCE_WINDOW_SECS);
        assert_eq!(
            segments[0].speaker_id, 10,
            "absent-speaker orphan relabeled to the present speaker"
        );
    }

    #[test]
    fn genuine_interjection_with_support_is_preserved() {
        // Short RIC between CYN and CARLOS, with another RIC within ±W.
        let mut segments = vec![
            seg(0.0, 20.0, 10),
            seg(20.0, 22.0, 30),
            seg(22.0, 40.0, 20),
            seg(50.0, 52.0, 30),
        ];
        relabel_temporal_orphans(&mut segments, MIN_PRESENCE_SECS, PRESENCE_WINDOW_SECS);
        assert_eq!(
            segments[1].speaker_id, 30,
            "short interjection WITH nearby same-label support is preserved"
        );
    }

    #[test]
    fn first_turn_with_only_future_support_is_preserved() {
        // t=0 short X with no PRIOR support, but X recurs at t=6 → not orphan.
        let mut segments = vec![
            seg(0.0, 2.0, 10),
            seg(2.0, 6.0, 20),
            seg(6.0, 8.0, 10),
            seg(8.0, 18.0, 30),
        ];
        relabel_temporal_orphans(&mut segments, MIN_PRESENCE_SECS, PRESENCE_WINDOW_SECS);
        assert_eq!(
            segments[0].speaker_id, 10,
            "symmetric ±W keeps a first-turn chunk that has future support"
        );
    }

    // ── diarization-clustering-perf — adversarial tests ───────────────────
    //
    // The new cached-matrix cluster_by_centroids must produce byte-for-byte
    // identical (labels, centroids) to the naive oracle across the full input
    // grid, handle edge cases without panic, respect the chunk cap, and reject
    // non-finite embeddings.

    fn make_chunk(embedding: Vec<f32>, duration_secs: f64) -> Chunk {
        Chunk {
            start_sample: 0,
            end_sample: 0,
            duration_secs,
            embedding,
        }
    }

    fn make_unit_chunk(i: usize, dim: usize, duration_secs: f64) -> Chunk {
        let mut e = vec![0.0f32; dim];
        if i < dim {
            e[i] = 1.0;
        }
        make_chunk(e, duration_secs)
    }

    fn random_unit_chunk(seed: u64, dim: usize, duration_secs: f64) -> Chunk {
        let mut rng_state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut e = vec![0.0f32; dim];
        let mut norm: f32 = 0.0;
        for v in e.iter_mut() {
            rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            let x = ((rng_state >> 33) as f64 / (1u64 << 31) as f64) as f32 * 2.0 - 1.0;
            *v = x;
            norm += x * x;
        }
        let norm = norm.sqrt().max(1e-12);
        for v in e.iter_mut() {
            *v /= norm;
        }
        make_chunk(e, duration_secs)
    }

    fn centroids_equal(a: &HashMap<u32, Vec<f32>>, b: &HashMap<u32, Vec<f32>>, eps: f32) -> bool {
        if a.len() != b.len() {
            return false;
        }
        for (k, va) in a {
            match b.get(k) {
                Some(vb) => {
                    if va.len() != vb.len() {
                        return false;
                    }
                    if va.iter().zip(vb.iter()).any(|(x, y)| (x - y).abs() > eps) {
                        return false;
                    }
                }
                None => return false,
            }
        }
        true
    }

    #[test]
    fn oracle_new_equals_naive_across_grid() {
        let dim = 8usize;
        let ns = [0usize, 1, 2, 5, 20, 80];
        let thresholds = [0.30f32, 0.40, 0.55];

        for &n in &ns {
            for &thr in &thresholds {
                let geometries = build_geometries(n, dim);
                for chunks in geometries {
                    let (new_labels, new_cent) = cluster_by_centroids(&chunks, thr);
                    let (old_labels, old_cent) = cluster_by_centroids_naive(&chunks, thr);
                    assert_eq!(
                        new_labels, old_labels,
                        "label mismatch at n={} thr={} (geometry of len {})",
                        n, thr, chunks.len()
                    );
                    assert!(
                        centroids_equal(&new_cent, &old_cent, 1e-5),
                        "centroid mismatch at n={} thr={}",
                        n, thr
                    );
                }
            }
        }
    }

    fn build_geometries(n: usize, dim: usize) -> Vec<Vec<Chunk>> {
        if n == 0 {
            return vec![vec![]];
        }
        let mut out = Vec::new();

        // Well-separated: two clusters of orthogonal basis vectors.
        if n >= 2 {
            let mut sep = Vec::new();
            for i in 0..n {
                let axis = i % 2;
                let mut e = vec![0.0f32; dim];
                if axis < dim {
                    e[axis] = 1.0;
                }
                sep.push(make_chunk(e, 3.0));
            }
            out.push(sep);
        }

        // Overlapping: random unit embeddings.
        let rand: Vec<Chunk> = (0..n).map(|i| random_unit_chunk(i as u64 + 1, dim, 3.0)).collect();
        out.push(rand);

        // All-identical.
        let identical_e = {
            let mut e = vec![0.0f32; dim];
            e[0] = 1.0;
            e
        };
        let identical: Vec<Chunk> = (0..n).map(|_| make_chunk(identical_e.clone(), 3.0)).collect();
        out.push(identical);

        // All-mutually-orthogonal (capped at dim axes).
        let ortho: Vec<Chunk> = (0..n).map(|i| make_unit_chunk(i, dim, 3.0)).collect();
        out.push(ortho);

        out
    }

    #[test]
    #[ignore = "n=5000 stress test: the O(n²·d) init + O(n³/3) matrix scan take ~90s \
                in plain f32 (no SIMD). D2's chunk cap (MAX_DIARIZATION_CHUNKS=600) \
                keeps production n ≤ 600 where clustering is sub-second. Run with \
                --ignored to verify no-OOM/no-panic at scale."]
    fn oversized_input_is_bounded() {
        let n = 5000usize;
        let dim = 192usize;
        let chunks: Vec<Chunk> = (0..n).map(|i| random_unit_chunk(i as u64, dim, 3.0)).collect();

        let (labels, centroids) = cluster_by_centroids(&chunks, 0.40);

        assert_eq!(labels.len(), n, "every chunk must be labelled");
        assert!(!centroids.is_empty(), "at least one cluster must exist");
    }

    #[test]
    fn oversized_matrix_stays_bounded_by_chunk_cap() {
        let dim = 8usize;
        let chunks: Vec<Chunk> = (0..MAX_DIARIZATION_CHUNKS)
            .map(|i| random_unit_chunk(i as u64, dim, 3.0))
            .collect();
        let (labels, centroids) = cluster_by_centroids(&chunks, 0.40);
        assert_eq!(labels.len(), MAX_DIARIZATION_CHUNKS);
        assert!(!centroids.is_empty());
    }

    // Perf-regression guard (diarization-clustering-perf task 1.2/2.3). The old
    // full-rescan AHC was O(n³); at the real freeze scale (n≈1640) it took 10+
    // minutes. The cached-matrix path is sub-second at the production cap. This
    // test runs at every `cargo test` (not #[ignore]) and fails if clustering
    // n=MAX_DIARIZATION_CHUNKS blows past the deadline — catching a regression
    // that reverts to full-rescan OR removes the cap. The deadline is 15 s, not
    // the 5 s an O(n³) revert would still clear: a debug build under `cargo
    // test` parallel load can spike 5× over its isolated runtime (~1 s here), so
    // the tighter bound flakes. 15 s stays ~60× under an O(n³) revert (minutes).
    #[test]
    fn production_scale_clustering_completes_under_wall_clock_deadline() {
        let dim = 8usize;
        let chunks: Vec<Chunk> = (0..MAX_DIARIZATION_CHUNKS)
            .map(|i| random_unit_chunk(i as u64, dim, 3.0))
            .collect();

        let start = std::time::Instant::now();
        let (labels, centroids) = cluster_by_centroids(&chunks, 0.40);
        let elapsed = start.elapsed();

        assert_eq!(labels.len(), MAX_DIARIZATION_CHUNKS);
        assert!(!centroids.is_empty());
        assert!(
            elapsed.as_secs() < 15,
            "n={} clustering took {:?} — regression past the cached-matrix O(n² log n) bound",
            MAX_DIARIZATION_CHUNKS,
            elapsed,
        );
    }

    #[test]
    fn empty_and_single_chunk_no_panic() {
        let (empty_labels, empty_cent) = cluster_by_centroids(&[], 0.40);
        assert!(empty_labels.is_empty());
        assert!(empty_cent.is_empty());

        let one = vec![make_chunk(vec![1.0, 0.0, 0.0], 3.0)];
        let (labels, cent) = cluster_by_centroids(&one, 0.40);
        assert_eq!(labels, vec![0u32]);
        assert_eq!(cent.len(), 1);
        assert!(cent.contains_key(&0));
    }

    #[test]
    fn degenerate_geometries_match_oracle() {
        let dim = 8;

        let identical_e = {
            let mut e = vec![0.0f32; dim];
            e[0] = 1.0;
            e
        };
        let identical: Vec<Chunk> = (0..10).map(|_| make_chunk(identical_e.clone(), 3.0)).collect();
        let (id_new, id_cent) = cluster_by_centroids(&identical, 0.40);
        let (id_old, _) = cluster_by_centroids_naive(&identical, 0.40);
        assert_eq!(id_new, id_old, "all-identical labels must match oracle");
        assert_eq!(id_cent.len(), 1, "all-identical must collapse to 1 cluster");

        let ortho: Vec<Chunk> = (0..dim).map(|i| make_unit_chunk(i, dim, 3.0)).collect();
        let (ort_new, _) = cluster_by_centroids(&ortho, 0.40);
        let (ort_old, _) = cluster_by_centroids_naive(&ortho, 0.40);
        assert_eq!(ort_new, ort_old, "all-orthogonal labels must match oracle");
        assert!(
            (0..dim).all(|i| ort_new[i] == i as u32),
            "all-orthogonal must produce n distinct clusters (no merges)"
        );
    }

    #[test]
    fn chunk_cap_formula_coarsens_long_meetings() {
        // build_chunks needs a real embedding extractor (model files) so the
        // integration is covered by the #[ignore] real-audio tests. The cap
        // arithmetic is a pure function of segment durations — pinned here.
        //
        // Long meeting: speech_seconds / 600 > 3.0 → effective_split coarsens.
        let long_speech = (MAX_DIARIZATION_CHUNKS as f64) * SPLIT_TARGET_SECS * 3.0; // 5400s
        let long_split = (long_speech / MAX_DIARIZATION_CHUNKS as f64).max(SPLIT_TARGET_SECS);
        assert_eq!(long_split, 9.0, "long meeting coarsens to speech/600");
        assert!(long_split > SPLIT_TARGET_SECS);
        assert!(
            (long_speech / long_split).floor() as usize <= MAX_DIARIZATION_CHUNKS,
            "coarsened granularity must keep chunk count ≤ cap"
        );
    }

    #[test]
    fn short_meetings_keep_default_granularity() {
        let short_speech = 600.0f64; // 10 min < 30 min cap threshold
        let short_split = (short_speech / MAX_DIARIZATION_CHUNKS as f64).max(SPLIT_TARGET_SECS);
        assert_eq!(short_split, SPLIT_TARGET_SECS, "short meetings keep 3.0s granularity");
        assert_eq!(
            (short_speech / short_split).floor() as usize,
            (short_speech / SPLIT_TARGET_SECS).floor() as usize,
            "short meeting chunk count is unchanged from today"
        );
    }

    #[test]
    fn non_finite_embeddings_do_not_corrupt_clustering() {
        // cosine_similarity of a NaN embedding yields NaN; the > threshold
        // predicate is false for NaN, so NaN chunks never merge but they also
        // never crash. The diarization pipeline rejects these upstream via
        // is_effectively_silent / the extractor's finite guard, so reaching
        // cluster_by_centroids with a NaN is itself a bug — but we verify the
        // function does not panic or produce a corrupted label array.
        let dim = 4;
        let mut chunks: Vec<Chunk> = (0..3).map(|i| make_unit_chunk(i, dim, 3.0)).collect();
        chunks.push(make_chunk(vec![f32::NAN; dim], 3.0));

        let (labels, _) = cluster_by_centroids(&chunks, 0.40);
        assert_eq!(labels.len(), 4, "all chunks labelled, no panic");
        // The NaN chunk must not merge into any cluster (NaN > threshold is false).
        let nan_label = labels[3];
        let others: Vec<u32> = labels[0..3].iter().copied().collect();
        assert!(!others.contains(&nan_label), "NaN chunk must not corrupt other clusters");
    }

    // Verification gap on diarization-clustering-perf: oracle_new_equals_naive
    // _across_grid only covers degenerate geometries (orthogonal basis vectors,
    // all-identical, fully-random unit vectors). Real nemo_titanet embeddings
    // produce 4–8 loosely separated clusters in 192-d with Gaussian within-
    // cluster noise — creating the intermediate similarities and near-tie merge
    // candidates that exercise the cached-matrix rescan path. If the perf
    // refactor is behavior-preserving on realistic structure, labels and
    // centroids must match byte-for-byte.

    fn lcg_uniform(seed: &mut u64) -> f64 {
        *seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((*seed >> 11) as f64) / ((1u64 << 53) as f64)
    }
    fn lcg_gaussian(seed: &mut u64) -> f32 {
        let u1 = lcg_uniform(seed).max(1e-12);
        let u2 = lcg_uniform(seed);
        let r = (-2.0 * u1.ln()).sqrt();
        let theta = 2.0 * std::f64::consts::PI * u2;
        (r * theta.cos()) as f32
    }

    #[test]
    fn cached_matrix_matches_naive_on_realistic_cluster_structure() {
        let dim = 192usize;
        let n_clusters = 4usize;
        let per_cluster = 50usize;
        // sigma=0.08 in 192-d: ||n||≈1.11, so within-cluster cosine ≈ 1/(1+1.23)≈0.45
        // and between-cluster ≈ 0/(1+1.23)=0 — straddling thr=0.40 so the loop hits
        // real merge-order decisions rather than trivially identical vectors.
        let noise_sigma: f32 = 0.08;
        let threshold = 0.40f32;

        let mut seed: u64 = 0xA11C_E5E1_4B6D_2F3A;

        // 4 random unit-vector centers in 192-d (mutual cosine ≈ 0).
        let mut centers: Vec<Vec<f32>> = Vec::with_capacity(n_clusters);
        for _ in 0..n_clusters {
            let mut c = vec![0.0f32; dim];
            let mut norm: f32 = 0.0;
            for v in c.iter_mut() {
                let x = (lcg_uniform(&mut seed) * 2.0 - 1.0) as f32;
                *v = x;
                norm += x * x;
            }
            let norm = norm.sqrt().max(1e-12);
            for v in c.iter_mut() {
                *v /= norm;
            }
            centers.push(c);
        }

        // Emit embeddings INTERLEAVED by cluster so members are not contiguous —
        // forces the matrix scan to cross cluster boundaries every iteration
        // and exercise the cached rescan under realistic near-tie candidates.
        // Built directly in interleaved order to avoid needing Chunk: Clone.
        let mut chunks: Vec<Chunk> = Vec::with_capacity(n_clusters * per_cluster);
        for i in 0..per_cluster {
            for c in 0..n_clusters {
                let center = &centers[c];
                let mut e = vec![0.0f32; dim];
                let mut norm: f32 = 0.0;
                for d in 0..dim {
                    let val = center[d] + lcg_gaussian(&mut seed) * noise_sigma;
                    e[d] = val;
                    norm += val * val;
                }
                let norm = norm.sqrt().max(1e-12);
                for v in e.iter_mut() {
                    *v /= norm;
                }
                // start_sample varies so chunks are distinguishable (mirrors
                // production where each chunk covers a distinct audio window).
                let start_sample = (i * n_clusters + c) * 48000;
                let mut chunk = make_chunk(e, 3.0);
                chunk.start_sample = start_sample;
                chunk.end_sample = start_sample + 48000;
                chunks.push(chunk);
            }
        }

        let (new_labels, new_cent) = cluster_by_centroids(&chunks, threshold);
        let (old_labels, old_cent) = cluster_by_centroids_naive(&chunks, threshold);

        // Sanity: the test is non-degenerate — it actually produced multiple
        // clusters (otherwise the equivalence check is vacuously true).
        let unique: std::collections::HashSet<u32> = new_labels.iter().copied().collect();
        assert!(
            (2..=n_clusters as u32 + 1).contains(&(unique.len() as u32)),
            "expected 2..={} clusters from synthetic structure, got {}; adjust noise_sigma",
            n_clusters,
            unique.len()
        );

        assert_eq!(
            new_labels, old_labels,
            "cached-matrix labels must equal naive oracle on realistic 4-cluster Gaussian structure \
             ({} embeddings, dim={}, sigma={}, thr={}); divergence means the perf refactor changed \
             clustering behavior, not just speed",
            chunks.len(),
            dim,
            noise_sigma,
            threshold,
        );
        assert!(
            centroids_equal(&new_cent, &old_cent, 1e-4),
            "cached-matrix centroids must match naive oracle within 1e-4 on realistic structure \
             (cluster count = {})",
            unique.len(),
        );
    }

    // ── diarization-temporal-coherence — adversarial tests (change tasks §1–§5) ──

    fn emb(axis: usize, dim: usize) -> Vec<f32> {
        let mut e = vec![0.0f32; dim];
        if axis < dim {
            e[axis] = 1.0;
        }
        e
    }

    fn ts_seq(n: usize, step: f64) -> Vec<f64> {
        (0..n).map(|i| i as f64 * step).collect()
    }

    fn centroids_from(map: &[(u32, Vec<f32>)]) -> HashMap<u32, Vec<f32>> {
        map.iter().cloned().collect()
    }

    fn unique_count(labels: &[u32]) -> usize {
        let s: std::collections::HashSet<u32> = labels.iter().copied().collect();
        s.len()
    }

    // Unit vector at cosine `cos` from the `voice_axis` direction, lying in the
    // (voice_axis, perp_axis) plane. Voice = emb(voice_axis) is also unit, so
    // cos(voice, this) = `cos` exactly — used to build drifted/realistic centroids
    // that stress the margin at a known cosine rather than the trivial cos 0.
    fn centroid_at_cos(voice_axis: usize, perp_axis: usize, cos: f32, dim: usize) -> Vec<f32> {
        let mut v = vec![0.0f32; dim];
        let s = (1.0_f32 - cos * cos).max(0.0).sqrt();
        if voice_axis < dim {
            v[voice_axis] = cos;
        }
        if perp_axis < dim {
            v[perp_axis] = s;
        }
        v
    }

    // Task 1.1 — flicker: [0,1,0,1,…] singleton runs collapse via the D3 floor.
    #[test]
    fn smooth_flicker_islands_collapse() {
        let dim = 4;
        let voice = emb(0, dim);
        let labels = vec![0u32, 1, 0, 1, 0, 1, 0, 1, 0];
        let embeddings = vec![voice.clone(); 9];
        let timestamps = ts_seq(9, 3.0);
        let durations = vec![3.0f64; 9];
        let centroids = centroids_from(&[(0, voice.clone()), (1, voice.clone())]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(unique_count(&out), 1, "flicker must collapse to one label: {:?}", out);
    }

    // Task 1.2 — contamination seed: a t=0 chunk mis-assigned to a spurious cluster
    // (1) is absorbed into its neighbours' cluster (0) by the neighbourhood vote.
    #[test]
    fn smooth_contamination_seed_absorbed_by_neighbours() {
        let dim = 4;
        let c0 = emb(0, dim);
        let c1 = emb(1, dim);
        let labels = vec![1u32, 0, 0, 0, 0, 0, 0];
        // chunk 0's embedding is genuinely the cluster-0 voice; global AHC mis-split it.
        let embeddings = vec![c0.clone(); 7];
        let timestamps = ts_seq(7, 3.0);
        let durations = vec![3.0f64; 7];
        let centroids = centroids_from(&[(0, c0), (1, c1)]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(out[0], 0, "contamination seed must be absorbed: {:?}", out);
    }

    // Task 1.3 — sustained absorption: a 20-chunk run mis-assigned to a drifted
    // cluster is recovered (≥80%) because its neighbours' voice votes the true cluster.
    #[test]
    fn smooth_sustained_absorption_recovered() {
        let dim = 4;
        let c = emb(0, dim);
        let d = emb(1, dim);
        let mut labels = vec![1u32; 3];
        labels.extend(vec![2u32; 20]);
        labels.extend(vec![1u32; 2]);
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> = (0..n).map(|_| c.clone()).collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(1, c), (2, d)]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        let recovered = (3..23).filter(|&i| out[i] == 1).count();
        assert!(
            recovered >= 16,
            "absorption recovery must be ≥80% (16/20), got {}/20: {:?}",
            recovered,
            out
        );
    }

    // Task 1.4 — real turn preserved: a substantial turn (≥ MIN_SMOOTH_SEGMENT_SECS)
    // between two DIFFERENT speakers is not smoothed away. Interior chunks have
    // unanimous same-label neighbours; boundary chunks see balanced support from
    // both sides so the confidence gate blocks the flip.
    #[test]
    fn smooth_real_turn_between_different_speakers_preserved() {
        let dim = 4;
        // 5 chunks (15s ≥ 10s floor) of speaker 1, flanked by speakers 0 and 2.
        let labels = vec![0u32, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2];
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> =
            (0..n).map(|i| emb(labels[i] as usize, dim)).collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(0, emb(0, dim)), (1, emb(1, dim)), (2, emb(2, dim))]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        let turn_preserved = (5..10).all(|i| out[i] == 1);
        assert!(turn_preserved, "substantial turn must be preserved: {:?}", out);
    }

    // Task 1.4b — short interjection preserved: a sub-floor run sandwiched between
    // two DIFFERENT speakers must NOT be merged away (would reintroduce the
    // "merged turns" failure). The damped self-vote anchors it; the split neighbour
    // votes on either side cannot beat it by the confidence margin.
    #[test]
    fn smooth_short_interjection_between_different_speakers_preserved() {
        let dim = 4;
        // chunk 5 (label 2, 3s < 10s floor) sits between a label-0 run and a label-1 run.
        let labels = vec![0u32, 0, 0, 0, 0, 2, 1, 1, 1, 1, 1];
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> =
            (0..n).map(|i| emb(labels[i] as usize, dim)).collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(0, emb(0, dim)), (1, emb(1, dim)), (2, emb(2, dim))]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(out[5], 2, "short interjection between two different speakers must be preserved: {:?}", out);
    }

    // Task 1.5 — degenerate embedding: a NaN/Inf neighbour is skipped (contributes
    // 0) and must not corrupt the vote or panic.
    #[test]
    fn smooth_nan_embedding_neighbour_skipped() {
        let dim = 4;
        let c0 = emb(0, dim);
        let c1 = emb(1, dim);
        let nan = vec![f32::NAN; dim];
        // chunk 1 (NaN embedding, label 1) sits between chunk 0 (mis-labelled 1)
        // and chunks 2..4 (label 0). The NaN must be skipped so chunk 0 still flips.
        let labels = vec![1u32, 1, 0, 0, 0];
        let embeddings = vec![c0.clone(), nan, c0.clone(), c0.clone(), c0.clone()];
        let timestamps = ts_seq(5, 3.0);
        let durations = vec![3.0f64; 5];
        let centroids = centroids_from(&[(0, c0), (1, c1)]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(out[0], 0, "NaN neighbour must be skipped, not corrupt the vote: {:?}", out);
    }

    // Task 1.6 — degenerate timestamp: a NaN/Inf timestamp excludes that chunk from
    // every window (unknowable order) and preserves its label; no panic.
    #[test]
    fn smooth_nan_timestamp_chunk_excluded_from_windows() {
        let dim = 4;
        let voice = emb(0, dim);
        let labels = vec![0u32, 0, 7, 0, 0]; // chunk 2 carries a sentinel label
        let embeddings = vec![voice.clone(); 5];
        let timestamps = vec![0.0, 3.0, f64::NAN, 9.0, 12.0];
        let durations = vec![3.0f64; 5];
        let centroids = centroids_from(&[(0, voice.clone()), (7, voice)]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(out.len(), 5);
        assert_eq!(out[2], 7, "non-finite-timestamp chunk must keep its label: {:?}", out);
    }

    // Task 1.7 — no-regression: a clean, well-separated meeting is a near-no-op
    // (the confidence gate blocks every flip; output == input).
    #[test]
    fn smooth_clean_meeting_is_noop() {
        let dim = 3;
        let labels = vec![0u32, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2];
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> =
            (0..n).map(|i| emb(labels[i] as usize, dim)).collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[
            (0, emb(0, dim)),
            (1, emb(1, dim)),
            (2, emb(2, dim)),
        ]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(out, labels, "clean meeting must be a no-op (confidence gate)");
    }

    // Task 1.8 — SAFETY invariants (not behavioral claims): for ANY label input,
    // smoothing never increases cluster count, never invents a new label, and
    // preserves length. A no-op would pass these too — by design. The behavioral
    // claims (absorption recovery, interjection preservation, clean-meeting
    // no-op, realistic-cosine variants) are pinned by the targeted tests above,
    // which is where the margin boundary actually gets exercised.
    proptest::proptest! {
        #[test]
        fn proptest_smoothing_invariants(
            labels in proptest::collection::vec(0u32..6u32, 1..60)
        ) {
            let n = labels.len();
            let k = (labels.iter().copied().max().unwrap_or(0) + 1) as usize;
            let dim = k.max(1);
            let embeddings: Vec<Vec<f32>> = (0..n).map(|i| emb(labels[i] as usize, dim)).collect();
            let centroids: HashMap<u32, Vec<f32>> = (0..k as u32)
                .map(|kk| (kk, emb(kk as usize, dim))).collect();
            let timestamps = ts_seq(n, 1.0);
            let durations = vec![1.0f64; n];
            let out = smooth_labels_temporal(
                &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
            );
            proptest::prop_assert_eq!(out.len(), n, "output length must equal input length");
            let in_unique: std::collections::HashSet<u32> = labels.iter().copied().collect();
            let out_unique: std::collections::HashSet<u32> = out.iter().copied().collect();
            proptest::prop_assert!(
                out_unique.len() <= in_unique.len(),
                "smoothing must not increase cluster count: {} > {}",
                out_unique.len(),
                in_unique.len()
            );
            for l in &out {
                proptest::prop_assert!(in_unique.contains(l), "smoothing invented new label {}", l);
            }
        }
    }

    // Task 2.1 — centroid recomputation: de-contaminated labels yield a centroid
    // different from the pre-smoothing (contaminated) one.
    #[test]
    fn recompute_centroids_reflects_cleaned_labels() {
        let dim = 2;
        let v0 = emb(0, dim);
        let v1 = emb(1, dim);
        // Contaminated: cluster 1 owns one v0 chunk + one v1 chunk → blended centroid.
        let contaminated_labels = vec![0u32, 0, 1, 1];
        let embeddings = vec![v0.clone(), v0.clone(), v0.clone(), v1.clone()];
        let contam = recompute_centroids_from_labels(&contaminated_labels, &embeddings, &[1.0; 4]);
        // Cleaned: the v0 chunk moves to cluster 0 → cluster 1 is purely v1.
        let cleaned_labels = vec![0u32, 0, 0, 1];
        let cleaned = recompute_centroids_from_labels(&cleaned_labels, &embeddings, &[1.0; 4]);
        assert_ne!(
            contam.get(&1), cleaned.get(&1),
            "cleaned centroid must differ from contaminated"
        );
        assert!(
            centroids_equal(
                &HashMap::from([(1u32, v1.clone())]),
                &HashMap::from([(1u32, cleaned.get(&1).unwrap().clone())]),
                1e-4,
            ),
            "cleaned cluster-1 centroid must equal the pure v1 voice"
        );
    }

    // Task 2.2 — phantom drop: a cluster whose chunks have zero total duration is
    // dropped, not preserved as a phantom centroid.
    #[test]
    fn recompute_centroids_drops_zero_duration_cluster() {
        let dim = 2;
        let v0 = emb(0, dim);
        let v1 = emb(1, dim);
        // cluster 1 chunk has duration 0 → no surviving duration.
        let labels = vec![0u32, 1];
        let embeddings = vec![v0, v1];
        let centroids = recompute_centroids_from_labels(&labels, &embeddings, &[1.0, 0.0]);
        assert!(centroids.contains_key(&0), "cluster 0 (duration > 0) must survive");
        assert!(!centroids.contains_key(&1), "zero-duration cluster must be dropped");
    }

    // Task 3.1 — fixed-point iteration: smooth_to_fixed_point recovers ≥80% of the
    // absorbed run (exercising vote → recompute → vote), and never recovers fewer
    // chunks than a single pass.
    #[test]
    fn smooth_fixed_point_recovers_absorption() {
        let dim = 4;
        let c = emb(0, dim);
        let d = emb(1, dim);
        let mut labels = vec![1u32; 3];
        labels.extend(vec![2u32; 20]);
        labels.extend(vec![1u32; 2]);
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> = (0..n).map(|_| c.clone()).collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(1, c.clone()), (2, d.clone())]);
        let (out, _) = smooth_to_fixed_point(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        let recovered = (3..23).filter(|&i| out[i] == 1).count();
        assert!(recovered >= 16, "fixed-point must recover ≥80%: {}/20", recovered);

        // single-pass recovery (for the non-inferior comparison)
        let single = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        let single_recovered = (3..23).filter(|&i| single[i] == 1).count();
        assert!(
            recovered >= single_recovered,
            "fixed-point ({} recovered) must not under-perform a single pass ({})",
            recovered,
            single_recovered
        );
    }

    // Task 3.2 — termination: the fixed-point loop always returns (max_iters cap),
    // and never increases cluster count.
    #[test]
    fn smooth_fixed_point_terminates_and_never_grows_clusters() {
        let dim = 3;
        // adversarial flicker that could loop if the floor re-created islands.
        let labels: Vec<u32> = (0..30).map(|i| (i % 3) as u32).collect();
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> = (0..n).map(|i| emb((i % 3) as usize, dim)).collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(0, emb(0, dim)), (1, emb(1, dim)), (2, emb(2, dim))]);
        let (out, recomputed) = smooth_to_fixed_point(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(out.len(), n, "output length preserved");
        assert!(
            unique_count(&out) <= unique_count(&labels),
            "cluster count must not grow"
        );
        // returned centroids must match the returned labels' label set
        for seg_label in out.iter().copied() {
            assert!(recomputed.contains_key(&seg_label), "centroid missing for label {}", seg_label);
        }
    }

    // Task 5.1 — scale: a 600-chunk meeting (~the production case) smooths in <1s.
    #[test]
    fn smooth_scales_sub_second_on_600_chunks() {
        let n = 600;
        let dim = 192;
        // 4 speakers with deterministic per-chunk voices cycling in long runs.
        let labels: Vec<u32> = (0..n).map(|i| ((i / 150) % 4) as u32).collect();
        let embeddings: Vec<Vec<f32>> =
            (0..n).map(|i| emb((i / 150) % 4, dim)).collect();
        let timestamps = ts_seq(n, 7.86);
        let durations = vec![7.86f64; n];
        let centroids = centroids_from(&[
            (0, emb(0, dim)),
            (1, emb(1, dim)),
            (2, emb(2, dim)),
            (3, emb(3, dim)),
        ]);
        let t = std::time::Instant::now();
        let (out, _) = smooth_to_fixed_point(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        let elapsed = t.elapsed().as_secs_f64();
        assert_eq!(out.len(), n);
        assert!(elapsed < 1.0, "600-chunk smoothing must complete <1s, took {:.3}s", elapsed);
    }

    // ── realistic-cosine variants (adversarial-review 2026-06-29: the orthogonal
    //    embeddings above make the margin trivially large, so they don't exercise
    //    the boundary. These use cos 0.6–0.95 to validate the mechanism where
    //    drift/overlap is realistic, and to lock the absorption boundary.) ──

    // The absorbed speaker's voice (unit, = centroid 1) is mis-assigned to a
    // drifted cluster whose centroid sits at cos 0.90 to the true voice — the
    // realistic moderate-drift case. Margin gap 0.10 > μ=0.03 → recovers ≥80%.
    #[test]
    fn smooth_realistic_drift_absorption_recovered() {
        let dim = 4;
        let voice = emb(0, dim);
        let drifted = centroid_at_cos(0, 1, 0.90, dim);
        let mut labels = vec![1u32; 3];
        labels.extend(vec![0u32; 20]);
        labels.extend(vec![1u32; 2]);
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> = (0..n).map(|_| voice.clone()).collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(0, drifted), (1, voice.clone())]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        let recovered = (3..23).filter(|&i| out[i] == 1).count();
        assert!(
            recovered >= 16,
            "realistic-drift (cos 0.90) absorption must recover ≥80% (16/20), got {}/20: {:?}",
            recovered,
            out
        );
    }

    // At cos 0.95 to the true voice the score gap is exactly 0.05 — the OLD
    // μ=0.05 boundary where recovery failed (finding #1). With μ=0.03 the gap
    // 0.05 > 0.03 fires, so the severe volume-absorption case now recovers.
    #[test]
    fn smooth_absorption_recovers_near_drift_boundary() {
        let dim = 4;
        let voice = emb(0, dim);
        let drifted = centroid_at_cos(0, 1, 0.95, dim);
        let mut labels = vec![1u32; 3];
        labels.extend(vec![0u32; 20]);
        labels.extend(vec![1u32; 2]);
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> = (0..n).map(|_| voice.clone()).collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(0, drifted), (1, voice.clone())]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        let recovered = (3..23).filter(|&i| out[i] == 1).count();
        assert!(
            recovered >= 16,
            "cos-0.95 drift (old μ=0.05 boundary) must now recover ≥80% under μ=0.03, got {}/20: {:?}",
            recovered,
            out
        );
    }

    // Two speakers at a realistic between-speaker cosine of 0.6 (not orthogonal).
    // The self-differential 0.6·(1−0.6)=0.24 ≫ μ=0.03 keeps every chunk on its
    // own label, so a clean realistic meeting is a no-op (finding #2: the
    // orthogonal clean-meeting test gave a trivially huge margin).
    #[test]
    fn smooth_realistic_cosine_clean_meeting_is_noop() {
        let dim = 2;
        let voice_a = emb(0, dim);
        let voice_b = centroid_at_cos(0, 1, 0.60, dim);
        let labels = {
            let mut l = vec![0u32; 8];
            l.extend(vec![1u32; 8]);
            l
        };
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> = (0..n)
            .map(|i| if labels[i] == 0 { voice_a.clone() } else { voice_b.clone() })
            .collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(0, voice_a), (1, voice_b)]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(
            out, labels,
            "clean realistic meeting (between-speaker cos 0.6) must be a no-op: {:?}",
            out
        );
    }

    // A short interjection whose voice is at cos 0.6 to BOTH flanking speakers
    // (realistic overlap, not orthogonal). Self (0.6) anchors it: it votes 1.0
    // for its own centroid vs 0.6 for each flank, so its own label wins outright
    // and the split neighbour votes cannot erase it (finding #2/#5).
    #[test]
    fn smooth_realistic_cosine_interjection_preserved() {
        let dim = 3;
        let vx = emb(0, dim); // (1,0,0)
        let va = centroid_at_cos(0, 1, 0.60, dim); // (0.6,0.8,0)
        let mut vb = vec![0.0f32; dim];
        vb[0] = 0.6;
        vb[1] = 0.3; // cos(vb,va)=0.36+0.24=0.6
        vb[2] = (1.0_f32 - 0.6 * 0.6 - 0.3 * 0.3).max(0.0).sqrt();
        let labels = vec![0u32, 0, 0, 0, 0, 2, 1, 1, 1, 1, 1];
        let n = labels.len();
        let embeddings: Vec<Vec<f32>> = (0..n)
            .map(|i| match labels[i] {
                0 => va.clone(),
                1 => vb.clone(),
                _ => vx.clone(),
            })
            .collect();
        let timestamps = ts_seq(n, 3.0);
        let durations = vec![3.0f64; n];
        let centroids = centroids_from(&[(0, va), (1, vb), (2, vx)]);
        let out = smooth_labels_temporal(
            &labels, &embeddings, &timestamps, &durations, &centroids, &SmoothParams::default(),
        );
        assert_eq!(
            out[5], 2,
            "realistic-cosine (0.6) interjection between two speakers must be preserved: {:?}",
            out
        );
    }

    // Task 5.3 — verify against the production meeting that motivated this change
    // (meeting-00000000-0000-4000-8000-000000000001, 3 speakers: Carlos/Bob/
    // Carol). #[ignore] because it needs the on-device nemo_titanet model + the
    // meeting's audio file, which a hermetic unit test cannot load. Manual recipe:
    //   1. prod SQLite is READ-ONLY (C:\Users\user\AppData\Roaming\
    //      com.meetily.ai\meeting_minutes.sqlite, ?mode=ro); never write to it.
    //   2. Export the meeting's audio.mp4 to a temp wav (16 kHz mono).
    //   3. Re-diarize via a throwaway binary that calls
    //      SherpaDiarizationAdapter::process() on that wav + the meeting's
    //      transcript segments, then inspect the resulting per-chunk labels.
    //   4. PASS criteria (the failures this change targets):
    //        - Carol labels present in min 30–70 (was 0 rows);
    //        - singleton-run flicker < 10 % in the min 30+ region (was 44–53 %);
    //        - no 5 s transcript-row fragments in the min 30–50 zone (median was
    //          5 s; expect it back near the clean 22 s median).
    // The unit-level guarantees (absorption recovery ≥ 80 %, interjection
    // preservation, clean-meeting no-op, < 1 s at 600 chunks) are covered by the
    // non-ignored tests above.
    #[ignore = "requires on-device sherpa model + prod audio; see recipe above"]
    #[test]
    fn smooth_verifies_prod_meeting_95db() {
        // Verify stub: the recipe above is the manual gate. Running it needs the
        // production audio asset on disk, which is not checked into the repo.
        eprintln!(
            "manual verify gate — see the ignore-reason recipe. \
             Prod DB: meeting-00000001-... (READ-ONLY)."
        );
    }
}
