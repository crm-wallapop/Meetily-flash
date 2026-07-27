#!/usr/bin/env python3
"""Generate the clip manifest for the embed-probe cosine-equivalence harness.

This script is the FIRST step of the subprocess harness described in
embed-probe-sherpa/src/main.rs. It produces the JSON manifest that BOTH
embed-probe-sherpa and embed-probe-ort consume as argv[1].

What it does
------------
1. Opens the Meetily SQLite DB read-only and fetches folder_path for the
   cde5c264 meeting (the same meeting the
   nemo_titanet_ort_cosine_equivalence test in
   frontend/src-tauri/tests/pyannote_ort_probe.rs uses).
2. Finds the audio file (audio.mp4 / .wav / .m4a / .mp3) in that folder.
3. Emits a manifest JSON with 10 clips — the SAME timestamps the test used
   (verbatim from pyannote_ort_probe.rs:1894-1910):
       2 silence   (DB transcript gaps, 10-14s of no speech)
       2 short <1s (sub-second speech windows from active regions)
       2 overlap   (banter rapid multi-turn)
       4 clean     (>20s single-speaker monologues)
   Each clip's start/end SECONDS is converted to start_sample/end_sample @
   16000 Hz (the rate sherpa's accept_waveform and the port's nemo pipeline
   both assume).
4. Writes the manifest to embed-probe-clips.json in the repo root.

Usage
-----
    python embed-probe-sherpa/gen_clips_manifest.py
    # -> writes <repo>/embed-probe-clips.json

    # Optional overrides:
    MEETING_ID=meeting-... DB_PATH=/path/to/db OUTPUT=/path/manifest.json \
        python embed-probe-sherpa/gen_clips_manifest.py

Dependencies: only the Python standard library (sqlite3, json, os, sys, pathlib).
No pip install needed.
"""

import json
import os
import sqlite3
import sys
from pathlib import Path

# --- Defaults (overridable via env) ---
DEFAULT_DB_PATH = r"C:\Users\CarlosRuizMartínez\AppData\Roaming\com.meetily.ai\meeting_minutes.sqlite"
DEFAULT_MEETING_ID = "meeting-cde5c264-1c4a-49d9-97c5-6a7e69bb9323"
SAMPLE_RATE = 16000  # sherpa + nemo both assume 16kHz mono f32

# --- Clip timestamps (VERBATIM from pyannote_ort_probe.rs:1894-1910) ---
# Each tuple: (start_secs, end_secs, id, label)
# The id is a slug derived from the label so compare_embeddings.py can join
# sherpa and ort results unambiguously.
CLIPS_SECS = [
    # Silence / near-silence (2) — drawn from the two largest DB transcript
    # gaps (no speech for 10-14s).
    (2905.30, 2908.30, "silence-1", "silence-1 (inside 13.9s gap 2905.3-2919.2)"),
    (1878.58, 1881.58, "silence-2", "silence-2 (inside 10.1s gap 1878.6-1888.7)"),
    # Short <1s clips (2) — sub-second speech windows from active regions.
    (6.00, 6.70, "short-1", "short-1 0.7s (banter onset)"),
    (1917.63, 1918.33, "short-2", "short-2 0.7s (inside 3.1s seg 1917.6-1920.7)"),
    # Overlap / dense regions (2) — banter rapid multi-turn.
    (5.67, 8.67, "overlap-1", "overlap-1 (banter rapid multi-turn)"),
    (10.00, 13.00, "overlap-2", "overlap-2 (banter rapid multi-turn)"),
    # Clean single-speaker regions (4) — drawn from >20s monologues.
    (57.78, 61.78, "clean-1", "clean-1 (22s monologue 57.8-80.1)"),
    (80.05, 84.05, "clean-2", "clean-2 (23s monologue 80.1-103.5)"),
    (1057.00, 1061.00, "clean-3", "clean-3 (Ricardo join region 17:37)"),
    (1933.56, 1938.52, "clean-4", "clean-4 (4.96s seg 1933.6-1938.5)"),
]

AUDIO_CANDIDATES = ["audio.mp4", "audio.wav", "audio.m4a", "audio.mp3"]


def fetch_folder_path(db_path: str, meeting_id: str) -> str:
    """Open the SQLite DB read-only and return folder_path for the meeting."""
    # URI mode=ro + immutable where possible to avoid locking the live DB.
    uri = f"file:{db_path}?mode=ro"
    conn = sqlite3.connect(uri, uri=True)
    try:
        row = conn.execute(
            "SELECT folder_path FROM meetings WHERE id = ?", (meeting_id,)
        ).fetchone()
    finally:
        conn.close()
    if row is None:
        raise SystemExit(
            f"no meeting row for id={meeting_id} in {db_path}"
        )
    folder = row[0]
    if not folder:
        raise SystemExit(f"meeting {meeting_id} has NULL/empty folder_path")
    return folder


def find_audio(folder: str) -> Path:
    folder_path = Path(folder)
    for name in AUDIO_CANDIDATES:
        cand = folder_path / name
        if cand.exists():
            return cand
    raise SystemExit(
        f"no audio file ({AUDIO_CANDIDATES}) found in {folder}"
    )


def build_manifest(audio_path: Path) -> list:
    """Build the manifest array from CLIPS_SECS + the resolved audio path.

    The Rust binaries (embed-probe-sherpa / embed-probe-ort) parse argv[1] as a
    JSON array of clip objects, so we emit a bare array (no wrapper object).
    Per-clip metadata (start_seconds/end_seconds/label) is kept inline for human
    readers; the binaries ignore keys they don't read.
    """
    results = []
    for start_s, end_s, cid, label in CLIPS_SECS:
        # Round to nearest sample (matches the Rust `as usize` truncation the
        # test uses at line 2055: `(start_s * sr_f) as usize`).
        start_sample = int(start_s * SAMPLE_RATE)
        end_sample = int(end_s * SAMPLE_RATE)
        results.append(
            {
                "id": cid,
                "path": str(audio_path),
                "start_sample": start_sample,
                "end_sample": end_sample,
                # Extra metadata for human readers; the Rust binaries ignore
                # unknown keys (they only read id/path/start_sample/end_sample).
                "start_seconds": start_s,
                "end_seconds": end_s,
                "label": label,
            }
        )
    return results


def main() -> int:
    db_path = os.environ.get("DB_PATH", DEFAULT_DB_PATH)
    meeting_id = os.environ.get("MEETING_ID", DEFAULT_MEETING_ID)

    if not Path(db_path).exists():
        raise SystemExit(f"DB not found: {db_path}")

    folder = fetch_folder_path(db_path, meeting_id)
    print(f"[manifest] meeting folder: {folder}", file=sys.stderr)

    audio_path = find_audio(folder)
    print(f"[manifest] audio file: {audio_path}", file=sys.stderr)

    manifest = build_manifest(audio_path)

    # Default output: embed-probe-clips.json in the repo root (parent of this
    # script's crate dir). Override with OUTPUT env var.
    repo_root = Path(__file__).resolve().parent.parent
    out_path = Path(os.environ.get("OUTPUT", repo_root / "embed-probe-clips.json"))
    out_path.write_text(json.dumps(manifest, indent=2), encoding="utf-8")
    print(f"[manifest] wrote {len(manifest)} clips to {out_path}", file=sys.stderr)
    print(f"[manifest]   (sherpa/ort binaries read argv[1] as a JSON array of clips)", file=sys.stderr)

    # Also print the manifest path to stdout so a shell pipeline can capture it.
    print(str(out_path))
    return 0


if __name__ == "__main__":
    sys.exit(main())
