//! Automated removal gate for tasks.md §4.2 — sherpa-onnx must stay out of
//! the PRODUCTION dependency graph and source. `embed-probe-sherpa` remains a
//! workspace member as the cosine-gate reference binary, so the dependency
//! check is SCOPED to the `meetily-flash` crate (`cargo tree -p`), not the
//! workspace root.
//!
//! Runs in every CI build; no model or recording needed.

use std::path::PathBuf;

fn source_rs_files(dir: &std::path::Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            source_rs_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn sherpa_removed_from_production_dependency_graph() {
    // Needles are assembled from fragments so this file does not contain the
    // literals it scans for (it is excluded from the source scan anyway).
    let dep_needles = [
        format!("{}-onnx", "sherpa"),
        format!("{}-onnx-sys", "sherpa"),
    ];
    let src_needles = [
        format!("Sherpa{}nx", "On"),   // type names: SherpaOnnx*
        format!("{}_onnx::", "sherpa"), // crate paths: sherpa_onnx::*
    ];

    // (1) Scoped dependency gate: `cargo tree -p meetily-flash`.
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = std::process::Command::new("cargo")
        .args(["tree", "-p", "meetily-flash"])
        .current_dir(&manifest_dir)
        .output()
        .expect("spawn cargo tree");
    assert!(
        out.status.success(),
        "cargo tree failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = String::from_utf8_lossy(&out.stdout);
    for needle in &dep_needles {
        assert!(
            !tree.contains(needle.as_str()),
            "'{needle}' re-entered the meetily-flash dependency graph:\n{tree}"
        );
    }

    // (2) Source gate: no sherpa references in src/ or tests/ (excluding this
    // file, which necessarily names the needles).
    let mut files = Vec::new();
    source_rs_files(&manifest_dir.join("src"), &mut files);
    source_rs_files(&manifest_dir.join("tests"), &mut files);
    assert!(
        !files.is_empty(),
        "source scan found no .rs files under src/ + tests/"
    );
    for path in files {
        if path.file_name().is_some_and(|n| n == "sherpa_removal_gate.rs") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        for needle in &src_needles {
            assert!(
                !content.contains(needle.as_str()),
                "'{needle}' found in {}: the sherpa removal gate forbids it",
                path.display()
            );
        }
    }
}
