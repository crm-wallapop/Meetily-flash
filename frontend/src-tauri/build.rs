#[path = "build/ffmpeg.rs"]
mod ffmpeg;

fn main() {
    // Point ort-sys to sherpa-onnx's ORT to avoid version conflicts
    configure_shared_ort();

    // GPU Acceleration Detection and Build Guidance
    detect_and_report_gpu_capabilities();

    #[cfg(target_os = "macos")]
    {
        println!("cargo:rustc-link-lib=framework=AVFoundation");
        println!("cargo:rustc-link-lib=framework=Cocoa");
        println!("cargo:rustc-link-lib=framework=Foundation");

        // Let the enhanced_macos crate handle its own Swift compilation
        // The swift-rs crate build will be handled in the enhanced_macos crate's build.rs
    }

    // Download and bundle FFmpeg binary at build-time
    ffmpeg::ensure_ffmpeg_binary();

    tauri_build::build()
}

/// Detects GPU acceleration capabilities and provides build guidance
fn detect_and_report_gpu_capabilities() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();

    println!("cargo:warning=🚀 Building Meetily for: {}", target_os);

    match target_os.as_str() {
        "macos" => {
            println!("cargo:warning=✅ macOS: Metal GPU acceleration ENABLED by default");
            #[cfg(feature = "coreml")]
            println!("cargo:warning=✅ CoreML acceleration ENABLED");
        }
        "windows" => {
            if cfg!(feature = "cuda") {
                println!("cargo:warning=✅ Windows: CUDA GPU acceleration ENABLED");
            } else if cfg!(feature = "vulkan") {
                println!("cargo:warning=✅ Windows: Vulkan GPU acceleration ENABLED");
            } else if cfg!(feature = "openblas") {
                println!("cargo:warning=✅ Windows: OpenBLAS CPU optimization ENABLED");
            } else {
                println!("cargo:warning=⚠️  Windows: Using CPU-only mode (no GPU or BLAS acceleration)");
                println!("cargo:warning=💡 For NVIDIA GPU: cargo build --release --features cuda");
                println!("cargo:warning=💡 For AMD/Intel GPU: cargo build --release --features vulkan");
                println!("cargo:warning=💡 For CPU optimization: cargo build --release --features openblas");

                // Try to detect NVIDIA GPU
                if which::which("nvidia-smi").is_ok() {
                    println!("cargo:warning=🎯 NVIDIA GPU detected! Consider rebuilding with --features cuda");
                }
            }
        }
        "linux" => {
            if cfg!(feature = "cuda") {
                println!("cargo:warning=✅ Linux: CUDA GPU acceleration ENABLED");
            } else if cfg!(feature = "vulkan") {
                println!("cargo:warning=✅ Linux: Vulkan GPU acceleration ENABLED");
            } else if cfg!(feature = "hipblas") {
                println!("cargo:warning=✅ Linux: AMD ROCm (HIP) acceleration ENABLED");
            } else if cfg!(feature = "openblas") {
                println!("cargo:warning=✅ Linux: OpenBLAS CPU optimization ENABLED");
            } else {
                println!("cargo:warning=⚠️  Linux: Using CPU-only mode (no GPU or BLAS acceleration)");
                println!("cargo:warning=💡 For NVIDIA GPU: cargo build --release --features cuda");
                println!("cargo:warning=💡 For AMD GPU: cargo build --release --features hipblas");
                println!("cargo:warning=💡 For other GPUs: cargo build --release --features vulkan");
                println!("cargo:warning=💡 For CPU optimization: cargo build --release --features openblas");

                // Try to detect NVIDIA GPU
                if which::which("nvidia-smi").is_ok() {
                    println!("cargo:warning=🎯 NVIDIA GPU detected! Consider rebuilding with --features cuda");
                }

                // Try to detect AMD GPU
                if which::which("rocm-smi").is_ok() {
                    println!("cargo:warning=🎯 AMD GPU detected! Consider rebuilding with --features hipblas");
                }
            }
        }
        _ => {
            println!("cargo:warning=ℹ️  Unknown platform: {}", target_os);
        }
    }

    // Performance guidance
    if !cfg!(feature = "cuda") && !cfg!(feature = "vulkan") && !cfg!(feature = "hipblas") && !cfg!(feature = "openblas") && target_os != "macos" {
        println!("cargo:warning=📊 Performance: CPU-only builds are significantly slower than GPU/BLAS builds");
        println!("cargo:warning=📚 See README.md for GPU/BLAS setup instructions");
    }
}

/// Both `sherpa-onnx-sys` (shared) and `ort-sys` bundle their own ONNX Runtime.
/// If both download different versions, the DLL loaded at runtime wins, causing API
/// version mismatches. This fn makes ort-sys reuse sherpa-onnx's ORT by setting
/// `ORT_LIB_LOCATION` and `ORT_SKIP_DOWNLOAD` before ort-sys runs its build script.
fn configure_shared_ort() {
    if std::env::var_os("ORT_LIB_LOCATION").is_some() {
        return;
    }

    let target_dir = std::env::var("CARGO_TARGET_DIR")
        .ok()
        .or_else(|| {
            let out_dir = std::env::var("OUT_DIR").ok()?;
            std::path::PathBuf::from(out_dir)
                .ancestors()
                .nth(3)
                .map(|p| p.display().to_string())
        })
        .unwrap_or_else(|| "target".to_string());

    let sherpa_prebuilt = std::path::Path::new(&target_dir)
        .join("sherpa-onnx-prebuilt");

    // Find the shared lib directory (name varies by version)
    if let Ok(entries) = std::fs::read_dir(&sherpa_prebuilt) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if name_str.contains("shared") && name_str.contains("lib") {
                let lib_dir = entry.path();
                // Check for onnxruntime.dll or onnxruntime.lib in lib/ subdirectory
                let lib_subdir = lib_dir.join("lib");
                let check_dir = if lib_subdir.exists() { &lib_subdir } else { &lib_dir };
                if check_dir.join("onnxruntime.lib").exists() || check_dir.join("onnxruntime.dll").exists() {
                    std::env::set_var("ORT_LIB_LOCATION", check_dir);
                    std::env::set_var("ORT_SKIP_DOWNLOAD", "1");
                    return;
                }
            }
        }
    }
}
