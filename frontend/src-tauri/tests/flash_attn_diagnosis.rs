// Vulkan flash attention diagnosis tests.
//
// Investigates whether garbled live transcription observed 2026-05-12 was caused by
// flash_attn=true on short VAD chunks, or by Whisper hallucinating on loud noise that
// the VAD forwarded incorrectly. See openspec/specs/audio-recording-quality/spec.md.
//
// Run all three with:
//   cargo test --test flash_attn_diagnosis --features vulkan -p meetily-flash -- --ignored --nocapture

use std::path::PathBuf;
use app_lib::audio::decoder::decode_audio_file;
use app_lib::whisper_engine::transcribe_raw;

fn model_path() -> Option<PathBuf> {
    let path = dirs::data_dir()?
        .join("com.meetily.ai")
        .join("models")
        .join("ggml-small-q5_1.bin");
    if path.exists() { Some(path) } else { None }
}

fn jfk_audio() -> Option<Vec<f32>> {
    let path = PathBuf::from("../../backend/whisper.cpp/samples/jfk.wav");
    if !path.exists() { return None; }
    decode_audio_file(&path).ok().map(|d| d.to_whisper_format())
}

fn lcg_noise(samples: usize, amplitude: f32) -> Vec<f32> {
    let mut lcg: u32 = 0xDEAD_BEEF;
    (0..samples).map(|_| {
        lcg = lcg.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (lcg as i32 as f32 / i32::MAX as f32) * amplitude
    }).collect()
}

fn check(label: &str, model: &PathBuf, audio: &[f32]) {
    println!("  --- {} ({} samples, {:.1}s) ---", label, audio.len(), audio.len() as f64 / 16000.0);
    let without = transcribe_raw(model, audio, false).unwrap_or_else(|e| format!("ERROR: {e}"));
    let with    = transcribe_raw(model, audio, true).unwrap_or_else(|e| format!("ERROR: {e}"));
    println!("  flash_attn=false : {:?}", without);
    println!("  flash_attn=true  : {:?}", with);
    if with.starts_with("ERROR") {
        println!("  FAIL: flash_attn=true errored");
    } else if !with.is_empty() {
        println!("  WARN: flash_attn=true produced output (possible hallucination): {:?}", with);
    } else {
        println!("  OK: flash_attn=true returned empty (expected)");
    }
}

/// Diagnosis: does flash_attn=true misbehave on short VAD-sized segments (2 s)?
///
/// If output is empty or garbled with flash_attn=true but coherent with flash_attn=false,
/// flash_attn is the cause of the 2026-05-12 regression.
/// If both produce coherent output, investigate audio_ctx or n_threads from the same commit.
#[test]
#[ignore]
fn test_flash_attn_short_segment() {
    let model = match model_path() {
        Some(p) => p,
        None => { eprintln!("SKIP: ggml-small-q5_1.bin not found"); return; }
    };
    let full_audio = match jfk_audio() {
        Some(a) => a,
        None => { eprintln!("SKIP: jfk.wav not found"); return; }
    };

    // 2 s — squarely in the live VAD chunk range (1–3 s)
    let short = &full_audio[..32_000.min(full_audio.len())];
    println!("  Short segment: {} samples ({:.1}s)", short.len(), short.len() as f64 / 16000.0);

    let without = transcribe_raw(&model, short, false).expect("flash_attn=false failed");
    let with    = transcribe_raw(&model, short, true).expect("flash_attn=true failed");

    println!("  flash_attn=false : {:?}", without);
    println!("  flash_attn=true  : {:?}", with);

    assert!(!without.is_empty(), "flash_attn=false produced empty output — check model/device");

    let lower = with.to_lowercase();
    let plausible = lower.contains("and") || lower.contains("so") || lower.contains("my")
        || lower.contains("fellow") || lower.contains("american");
    if with.is_empty() {
        println!("  VERDICT: flash_attn=true → EMPTY — flash_attn is the cause");
    } else if !plausible {
        println!("  VERDICT: flash_attn=true → GARBLED ({:?}) — flash_attn is the cause", with);
    } else {
        println!("  VERDICT: flash_attn=true → coherent — flash_attn is NOT the cause; check audio_ctx/n_threads");
    }
}

/// Diagnosis: does Whisper hallucinate on non-speech noise input regardless of flash_attn?
///
/// Tests digital silence, -40 dBFS (microphone noise floor), and -6 dBFS (louder than speech,
/// matching the loud environmental noise observed 2026-05-12).
/// If flash_attn=false also produces garbled output on the -6 dBFS case, the root cause is
/// VAD mis-firing on loud noise — not flash_attn — and flash_attn can be re-enabled for Vulkan.
#[test]
#[ignore]
fn test_flash_attn_noise_inputs() {
    let model = match model_path() {
        Some(p) => p,
        None => { eprintln!("SKIP: ggml-small-q5_1.bin not found"); return; }
    };

    check("digital silence",                   &model, &vec![0.0f32; 32_000]);
    check("white noise -40 dBFS",              &model, &lcg_noise(32_000, 0.01));
    check("white noise -6 dBFS (> speech)",    &model, &lcg_noise(32_000, 0.5));
}

/// Baseline: flash_attn=true on a full 11 s segment (already confirmed correct 2026-05-13).
/// Re-run to confirm the model and device are healthy before interpreting the other results.
#[test]
#[ignore]
fn test_flash_attn_full_segment_baseline() {
    let model = match model_path() {
        Some(p) => p,
        None => { eprintln!("SKIP: ggml-small-q5_1.bin not found"); return; }
    };
    let audio = match jfk_audio() {
        Some(a) => a,
        None => { eprintln!("SKIP: jfk.wav not found"); return; }
    };

    println!("  Full segment: {} samples ({:.1}s)", audio.len(), audio.len() as f64 / 16000.0);

    let without = transcribe_raw(&model, &audio, false).expect("flash_attn=false failed");
    let with    = transcribe_raw(&model, &audio, true).expect("flash_attn=true failed");

    println!("  flash_attn=false : {:?}", without);
    println!("  flash_attn=true  : {:?}", with);

    assert!(!without.is_empty(), "flash_attn=false empty — check model/device");
    let lower = with.to_lowercase();
    let plausible = lower.contains("american") || lower.contains("country") || lower.contains("ask");
    println!("  Content check: {}", if plausible { "PASS" } else { "WARN (unexpected content)" });
}
