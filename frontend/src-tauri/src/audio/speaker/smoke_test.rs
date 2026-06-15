use anyhow::Result;

use super::diarization::DiarizationPort; // trait in scope for .process()
use super::sherpa_adapter::SherpaOnnxDiarizationAdapter;
use crate::audio::decoder::decode_audio_file;

fn models_dir() -> String {
    dirs::home_dir()
        .expect("home directory")
        .join(".meetily-models")
        .to_str()
        .expect("utf-8 path")
        .to_string()
}

fn meeting_audio_path() -> String {
    dirs::home_dir()
        .expect("home directory")
        .join("Music")
        .join("meetily-recordings")
        .join("Meeting 2026-01-01_00-00-00_2026-01-01_00-00")
        .join("audio.mp4")
        .to_str()
        .expect("utf-8 path")
        .to_string()
}

#[test]
#[ignore]
fn diarization_smoke_test() -> Result<()> {
    let models = models_dir();
    let embedding_path = format!("{}/nemo-titanet-embedding.onnx", models);
    let segmentation_path = format!("{}/pyannote-segmentation.onnx", models);
    let audio_path = meeting_audio_path();

    println!("=== Speaker Diarization Smoke Test ===");
    println!();

    // 1. Verify model files exist
    let emb = std::path::Path::new(&embedding_path);
    let seg = std::path::Path::new(&segmentation_path);
    let audio = std::path::Path::new(&audio_path);

    println!(
        "Embedding model: {} ({})",
        emb.display(),
        if emb.exists() { "OK" } else { "MISSING" }
    );
    println!(
        "Segmentation model: {} ({})",
        seg.display(),
        if seg.exists() { "OK" } else { "MISSING" }
    );
    println!(
        "Audio file: {} ({})",
        audio.display(),
        if audio.exists() { "OK" } else { "MISSING" }
    );
    println!();

    assert!(emb.exists(), "embedding model not found: {}", embedding_path);
    assert!(seg.exists(), "segmentation model not found: {}", segmentation_path);
    assert!(audio.exists(), "audio file not found: {}", audio_path);

    // 2. Decode audio to mono f32
    println!("Decoding audio...");
    let decoded = decode_audio_file(audio)?;
    println!(
        "Decoded: {}Hz, {}ch, {:.1}s, {} samples",
        decoded.sample_rate,
        decoded.channels,
        decoded.duration_seconds,
        decoded.samples.len()
    );

    let samples = decoded.to_whisper_format();
    println!("Whisper format: 16kHz mono, {} samples", samples.len());

    // 3. Create diarization adapter
    println!("\nLoading diarization models...");
    let adapter = SherpaOnnxDiarizationAdapter::new(&embedding_path, &segmentation_path)?;

    // 4. Run diarization
    println!("Running diarization on {:.0}s of audio...", decoded.duration_seconds);
    let start = std::time::Instant::now();
    let output = adapter.process(&samples, 16000, &[])?;
    let segments = output.segments;
    let elapsed = start.elapsed();

    println!("Diarization completed in {:.1}s", elapsed.as_secs_f64());
    println!();

    // 5. Report results
    if segments.is_empty() {
        println!("No speaker segments found.");
        return Ok(());
    }

    let num_speakers: std::collections::HashSet<u32> =
        segments.iter().map(|s| s.speaker_id).collect();

    println!("=== Results ===");
    println!("Speakers detected: {}", num_speakers.len());
    println!("Segments: {}", segments.len());
    println!();

    println!("{:<8} {:>10} {:>10} {:>10}", "Speaker", "Start(s)", "End(s)", "Dur(s)");
    println!("{}", "-".repeat(42));
    for seg in &segments {
        let dur = seg.end_seconds - seg.start_seconds;
        println!(
            "S{:<7} {:>10.2} {:>10.2} {:>10.2}",
            seg.speaker_id, seg.start_seconds, seg.end_seconds, dur
        );
    }

    // 6. Summary per speaker
    println!();
    println!("{:<10} {:>10} {:>12}", "Speaker", "Segments", "Total(s)");
    println!("{}", "-".repeat(34));
    for &sid in &num_speakers {
        let speaker_segs: Vec<_> = segments.iter().filter(|s| s.speaker_id == sid).collect();
        let total: f64 = speaker_segs
            .iter()
            .map(|s| s.end_seconds - s.start_seconds)
            .sum();
        println!("Speaker {:<3} {:>10} {:>12.1}", sid, speaker_segs.len(), total);
    }

    Ok(())
}
