// Transcription pipeline performance baseline.
//
// Run with:
//   cargo test --test pipeline_perf --features vulkan -- --ignored --nocapture
//
// Configuration via environment variables (all optional, fall back to constants):
//   PERF_MODEL       model name (e.g. "large-v3-turbo-q5_0", "small-q5_1")
//   PERF_VAD_MS      VAD redemption window in ms (e.g. "500", "2000", "5000")
//   PERF_RUN_ID      label written to the results JSON (e.g. "cycle1-debug")
//   PERF_RESULTS_DIR directory to write <RUN_ID>.json (defaults to AUDIO_FILE parent)
//
// Records wall-clock time for each stage so before/after comparisons are easy.
// Marked #[ignore] because it needs real hardware and external files.

use std::path::PathBuf;
use std::time::Instant;

use app_lib::audio::{decode_audio_file, vad::get_speech_chunks};
use app_lib::whisper_engine::WhisperEngine;

const AUDIO_FILE: &str = r"C:\Users\CarlosRuizMartínez\Music\meetily-recordings\Meeting 2026-05-08_10-05-12_2026-05-08_08-05\audio.mp4";
const MODELS_DIR: &str = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\models";
const DEFAULT_MODEL: &str = "large-v3-turbo-q5_0";
const DEFAULT_LANGUAGE: &str = "es";
const DEFAULT_VAD_REDEMPTION_MS: u32 = 2000;
const RESULTS_DIR: &str = r"C:\Users\CarlosRuizMartínez\Music\meetily-recordings\perf-sweep";

fn env_str(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_u32(key: &str, default: u32) -> u32 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

#[tokio::test(flavor = "multi_thread")]
#[ignore]
async fn bench_transcription_pipeline() {
    let model = env_str("PERF_MODEL", DEFAULT_MODEL);
    let language = env_str("PERF_LANGUAGE", DEFAULT_LANGUAGE);
    let vad_ms = env_u32("PERF_VAD_MS", DEFAULT_VAD_REDEMPTION_MS);
    let run_id = env_str("PERF_RUN_ID", &format!("{model}-vad{vad_ms}"));
    let results_dir = env_str("PERF_RESULTS_DIR", RESULTS_DIR);

    println!("=== bench_transcription_pipeline ===");
    println!("  run_id  : {run_id}");
    println!("  model   : {model}");
    println!("  language: {language}");
    println!("  vad_ms  : {vad_ms}");
    println!("  results : {results_dir}");
    println!();

    std::fs::create_dir_all(&results_dir).expect("create results dir");

    // 1. Decode
    let t = Instant::now();
    let decoded = decode_audio_file(std::path::Path::new(AUDIO_FILE))
        .expect("decode failed");
    let decode_ms = t.elapsed().as_millis();
    println!(
        "[1] DECODE     {:>6}ms  ({:.1}s audio, {}ch @ {}Hz)",
        decode_ms, decoded.duration_seconds, decoded.channels, decoded.sample_rate
    );

    // 2. Resample to 16 kHz mono
    let t = Instant::now();
    let samples = decoded.to_whisper_format();
    let resample_ms = t.elapsed().as_millis();
    println!(
        "[2] RESAMPLE   {:>6}ms  ({} samples @ 16kHz)",
        resample_ms,
        samples.len()
    );

    // 3. VAD segmentation
    let t = Instant::now();
    let segments = get_speech_chunks(&samples, vad_ms).expect("VAD failed");
    let vad_ms_elapsed = t.elapsed().as_millis();
    println!(
        "[3] VAD        {:>6}ms  ({} speech segments, redemption={}ms)",
        vad_ms_elapsed,
        segments.len(),
        vad_ms
    );

    // 4. Load engine + model
    let t = Instant::now();
    let engine = WhisperEngine::new_with_models_dir(Some(PathBuf::from(MODELS_DIR)))
        .expect("engine init failed");
    engine.discover_models().await.expect("model discovery failed");
    engine.load_model(&model).await.expect("model load failed");
    let load_ms = t.elapsed().as_millis();
    println!("[4] MODEL LOAD {:>6}ms  ({})", load_ms, model);

    // 5. Sequential transcription
    let t = Instant::now();
    let mut seg_rows: Vec<String> = Vec::new();  // for JSON
    let mut full_text_parts: Vec<String> = Vec::new();
    let mut total_conf = 0f64;
    let mut conf_count = 0usize;

    for (i, seg) in segments.iter().enumerate() {
        let dur_s = seg.samples.len() as f64 / 16000.0;
        let seg_t = Instant::now();
        let (text, conf, _) = engine
            .transcribe_audio_with_confidence(seg.samples.clone(), Some(language.clone()))
            .await
            .expect("transcription failed");
        let seg_ms = seg_t.elapsed().as_millis();
        let rtf = if dur_s > 0.0 { seg_ms as f64 / (dur_s * 1000.0) } else { 0.0 };
        let preview: String = text.chars().take(80).collect();
        println!(
            "    seg {:>3}  {:>6.1}s  {:>6}ms  rtf={:.2}  {:?}",
            i, dur_s, seg_ms, rtf, preview
        );
        if !text.trim().is_empty() {
            full_text_parts.push(text.trim().to_string());
            total_conf += conf as f64;
            conf_count += 1;
        }
        seg_rows.push(format!(
            r#"{{"i":{i},"dur_s":{dur_s:.2},"ms":{seg_ms},"rtf":{rtf:.3},"conf":{conf:.3},"words":{}}}"#,
            text.split_whitespace().count()
        ));
    }

    let transcribe_ms = t.elapsed().as_millis();
    let realtime = (decoded.duration_seconds * 1000.0) / transcribe_ms as f64;
    let avg_conf = if conf_count > 0 { total_conf / conf_count as f64 } else { 0.0 };
    let full_text = full_text_parts.join(" ");
    let word_count = full_text.split_whitespace().count();

    println!(
        "[5] TRANSCRIBE {:>6}ms  ({} segs, {:.2}x realtime, avg_conf={:.3}, {} words)",
        transcribe_ms, segments.len(), realtime, avg_conf, word_count
    );
    println!(
        "\nTOTAL (incl. model load): {}ms",
        decode_ms + resample_ms + vad_ms_elapsed + load_ms + transcribe_ms
    );
    println!(
        "TOTAL (excl. model load): {}ms",
        decode_ms + resample_ms + vad_ms_elapsed + transcribe_ms
    );

    // 6. Write transcript text file
    let transcript_path = format!("{results_dir}\\{run_id}.txt");
    std::fs::write(&transcript_path, &full_text).expect("write transcript");
    println!("\nTranscript → {transcript_path}");

    // 7. Write JSON metrics
    let audio_dur_s = decoded.duration_seconds;
    let segs_json = seg_rows.join(",\n    ");
    let json = format!(
        r#"{{
  "run_id": "{run_id}",
  "model": "{model}",
  "language": "{language}",
  "vad_redemption_ms": {vad_ms},
  "audio_duration_s": {audio_dur_s:.1},
  "segment_count": {seg_count},
  "decode_ms": {decode_ms},
  "resample_ms": {resample_ms},
  "vad_ms": {vad_ms_elapsed},
  "load_ms": {load_ms},
  "transcribe_ms": {transcribe_ms},
  "total_ms": {total_ms},
  "realtime_factor": {realtime:.3},
  "avg_confidence": {avg_conf:.4},
  "word_count": {word_count},
  "segments": [
    {segs_json}
  ]
}}"#,
        seg_count = segments.len(),
        total_ms = decode_ms + resample_ms + vad_ms_elapsed + load_ms + transcribe_ms
    );
    let json_path = format!("{results_dir}\\{run_id}.json");
    std::fs::write(&json_path, &json).expect("write json");
    println!("Metrics    → {json_path}");

    // 8. Print first/last lines as sanity check
    let words: Vec<&str> = full_text.split_whitespace().collect();
    let head: String = words.iter().take(30).cloned().collect::<Vec<_>>().join(" ");
    let tail: String = words.iter().rev().take(30).collect::<Vec<_>>().into_iter().rev().cloned().collect::<Vec<_>>().join(" ");
    println!("\n--- Transcript head ---");
    println!("{head}");
    println!("--- Transcript tail ---");
    println!("{tail}");
    println!("--- End ---");
}
