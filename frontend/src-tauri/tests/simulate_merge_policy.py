"""Offline simulator for merge_short_speakers consolidation policies.

Loads the pre-merge snapshot produced by the MEETIFY_MERGE_DUMP probe
(cluster durations, pre-merge segments, short->long centroid similarity
matrix) and evaluates candidate consolidation policies against the
cde5c264 ground truth:

  G1. exactly 3 speakers (user-confirmed)
  G2. banter window [5.67, 30.0] carries >= 2 distinct speakers
  G3. the segment covering 2818s (Ricardo, 46:58) has a different speaker
      from its temporal neighbours (>= 2700s, >= 2900s)
  G4. sanity bound: speaker count <= 5 (noise control)

Usage:
  python simulate_merge_policy.py <path-to-premerge.json>
"""

import json
import sys
from collections import defaultdict


def load(path):
    with open(path, encoding="utf-8") as f:
        return json.load(f)


def speaker_durations(segs):
    d = defaultdict(float)
    for s in segs:
        d[s["speaker"]] += s["end"] - s["start"]
    return d


def apply_policy(data, min_frac, min_abs, sim_gate):
    """Re-run consolidation from the PRE-merge state under a candidate policy.

    Mirrors merge_short_speakers: short clusters merge into their NEAREST
    (highest-cosine) long cluster; under a sim_gate, a short cluster with no
    long neighbour above the gate is KEPT as its own speaker instead.
    Returns (segments, final_speaker_set).
    """
    segs = [dict(s) for s in data["segments"]]
    durs = speaker_durations(segs)
    total = data["total_audio_secs"]
    min_dur = max(min_frac * total, min_abs)

    sims = {e["short"]: {t: s for t, s in e["sims"]} for e in data["short_long_sims"]}

    short = [sid for sid, d in durs.items() if d < min_dur]
    long_ = [sid for sid, d in durs.items() if d >= min_dur]

    remap = {}
    for sid in short:
        candidates = sims.get(sid, {})
        if not candidates:
            continue
        target, best_sim = max(candidates.items(), key=lambda kv: kv[1])
        if sim_gate is not None and best_sim < sim_gate:
            continue  # distinct voice: keep
        remap[sid] = target

    for s in segs:
        s["speaker"] = remap.get(s["speaker"], s["speaker"])
    return segs, set(s["speaker"] for s in segs)


def evaluate(name, segs, speakers):
    banter = {s["speaker"] for s in segs if s["start"] < 30.0 and s["end"] > 5.67}
    cov = [s for s in segs if s["start"] <= 2818.0 <= s["end"]]
    before = {s["speaker"] for s in segs if s["start"] <= 2700.0 <= s["end"] or s["start"] <= 2900.0 <= s["end"]}
    interj_distinct = bool(cov) and not (set(c["speaker"] for c in cov) & before)

    checks = {
        "G1_count3": len(speakers) == 3,
        "G2_banter>=2": len(banter) >= 2,
        "G3_interject": interj_distinct,
        "G4_count<=5": len(speakers) <= 5,
    }
    print(f"--- {name}")
    print(f"    speakers: {sorted(speakers)}")
    print(f"    banter speakers: {sorted(banter)}")
    print(f"    2818 covered by: {[c['speaker'] for c in cov]}  neighbours: {sorted(before)}")
    for k, v in checks.items():
        print(f"    {'PASS' if v else 'FAIL'}  {k}")
    return all(checks.values())


def main():
    data = load(sys.argv[1])
    print(f"min_dur (current policy) = {data['min_dur_secs']:.1f}s of {data['total_audio_secs']:.0f}s total")
    print(f"clusters pre-merge: {len(data['cluster_durations'])}")
    shorts = [c for c in data["cluster_durations"] if c["short"]]
    print(f"short clusters (< floor): {len(shorts)}")
    for c in sorted(data["cluster_durations"], key=lambda x: x["total_secs"]):
        tag = " [SHORT]" if c["short"] else ""
        print(f"  cluster {c['speaker']:>2}: {c['total_secs']:7.1f}s{tag}")

    results = {}
    results["P0 current (2% floor, no gate)"] = apply_policy(data, 0.02, 1.5, None)
    results["P1 floor 0.5% (~25s), no gate"] = apply_policy(data, 0.005, 1.5, None)
    results["P2 current floor + gate 0.45"] = apply_policy(data, 0.02, 1.5, 0.45)
    results["P2b current floor + gate 0.35"] = apply_policy(data, 0.02, 1.5, 0.35)
    results["P3 floor 0.5% + gate 0.45"] = apply_policy(data, 0.005, 1.5, 0.45)

    print()
    winners = []
    for name, (segs, speakers) in results.items():
        if evaluate(name, segs, speakers):
            winners.append(name)

    print()
    if winners:
        print("POLICIES SATISFYING ALL GROUND TRUTH:")
        for w in winners:
            print(f"  OK  {w}")
    else:
        print("NO CANDIDATE POLICY SATISFIES ALL GROUND TRUTH — design iteration needed.")


if __name__ == "__main__":
    main()
