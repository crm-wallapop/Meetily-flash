//! Title-extractor adapters. Ungated by design: pure regex over
//! `BrowserWindow.title`, no Win32 deps — unlike sibling `signaling` /
//! `browser_process`, which call Win32 APIs and are Windows-gated. Same
//! rationale as `google_cidrs` (pure data, also ungated). A future macOS
//! adapter ships here without editing `detection/mod.rs`.

pub mod meet;
