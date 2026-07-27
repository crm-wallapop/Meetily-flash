#!/usr/bin/env python3
"""Compare sherpa vs ort embedding JSON outputs for the cosine-equivalence gate.

This is the THIRD step of the subprocess harness described in
embed-probe-sherpa/src/main.rs. Both probe binaries emit the same schema:
    { "results": [
        { "id": "...", "embedding": [f32,...], "skipped": false },
        { "id": "...", "embedding": [],        "skipped": true }, ...
    ] }

This script:
  1. Reads the two JSON result files (sherpa + ort) from argv.
  2. For each clip id present in both AND not skipped on either side, computes
     cosine similarity of the two embedding vectors.
  3. Prints a per-clip table: id | sherpa_norm | ort_norm | cosine | PASS/FAIL.
  4. Prints a summary: min/mean/max cosine, overall PASS/FAIL.
  5. Exit code 0 if all cosines > 0.99, else 1.

Skipped clips are reported but excluded from the cosine stats (they are not
gate failures — see the test's `BOTH=None` branch at pyannote_ort_probe.rs:2117,
which the comment explicitly calls "not a cosine failure"). A clip skipped on
ONE side only IS a failure (the paths disagree on what's processable).

Usage
-----
    python compare_embeddings.py <sherpa_results.json> <ort_results.json>
    # exit 0 = all matching clips agree (cosine > 0.99)
    # exit 1 = at least one clip disagrees or one-sided skip

Threshold override:
    GATE_THRESHOLD=0.95 python compare_embeddings.py sherpa.json ort.json

Dependencies: Python standard library only.
"""

import json
import math
import os
import sys

DEFAULT_THRESHOLD = 0.99


def cosine(a, b):
    """Cosine similarity. Returns 0.0 if either vector is zero-norm."""
    if len(a) != len(b):
        raise ValueError(f"embedding dim mismatch: {len(a)} vs {len(b)}")
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(x * x for x in b))
    if na == 0.0 or nb == 0.0:
        return 0.0
    return dot / (na * nb)


def load_results(path):
    with open(path, "r", encoding="utf-8") as f:
        data = json.load(f)
    # Accept both {"results": [...]} and a bare [...].
    rows = data["results"] if isinstance(data, dict) else data
    out = {}
    for r in rows:
        out[r["id"]] = {
            "embedding": r.get("embedding", []),
            "skipped": bool(r.get("skipped", False)),
        }
    return out


def main() -> int:
    if len(sys.argv) != 3:
        print(f"usage: {sys.argv[0]} <sherpa_results.json> <ort_results.json>", file=sys.stderr)
        return 2
    sherpa_path, ort_path = sys.argv[1], sys.argv[2]
    threshold = float(os.environ.get("GATE_THRESHOLD", DEFAULT_THRESHOLD))

    sherpa = load_results(sherpa_path)
    ort = load_results(ort_path)

    ids = list(dict.fromkeys(list(sherpa.keys()) + list(ort.keys())))  # union, stable order

    print(f"# cosine-equivalence gate (threshold cosine > {threshold})")
    print(f"# sherpa: {sherpa_path} ({len(sherpa)} clips)")
    print(f"# ort:    {ort_path} ({len(ort)} clips)")
    print()

    header = f"{'id':<14} {'sherpa_norm':>12} {'ort_norm':>12} {'cosine':>12}  {'verdict':<8}"
    print(header)
    print("-" * len(header))

    cosines = []  # only clips where BOTH produced embeddings
    overall_pass = True
    mismatches = 0
    skipped_both = 0
    one_sided = 0

    for cid in ids:
        s = sherpa.get(cid)
        o = ort.get(cid)
        if s is None or o is None:
            where = "sherpa" if s is None else "ort"
            print(f"{cid:<14} {'--':>12} {'--':>12} {'--':>12}  MISSING in {where}")
            overall_pass = False
            mismatches += 1
            continue

        if s["skipped"] and o["skipped"]:
            # Both agree the clip is unprocessable — not a gate failure.
            print(f"{cid:<14} {'skip':>12} {'skip':>12} {'N/A':>12}  BOTH=skip")
            skipped_both += 1
            continue

        if s["skipped"] != o["skipped"]:
            # One-sided skip: paths disagree on processability → failure.
            which = "sherpa=skip" if s["skipped"] else "ort=skip"
            other = "ort=Some" if s["skipped"] else "sherpa=Some"
            print(f"{cid:<14} {'skip':>12} {'skip':>12} {'N/A':>12}  ONE-SIDED {which} {other}")
            overall_pass = False
            one_sided += 1
            continue

        # Both produced embeddings.
        es, eo = s["embedding"], o["embedding"]
        if len(es) != len(eo):
            print(f"{cid:<14} {'--':>12} {'--':>12} {'--':>12}  DIM MISMATCH {len(es)} vs {len(eo)}")
            overall_pass = False
            mismatches += 1
            continue
        sn = math.sqrt(sum(x * x for x in es))
        on = math.sqrt(sum(x * x for x in eo))
        c = cosine(es, eo)
        cosines.append(c)
        verdict = "PASS" if c > threshold else "FAIL"
        if c <= threshold:
            overall_pass = False
        print(f"{cid:<14} {sn:>12.4f} {on:>12.4f} {c:>12.6f}  {verdict}")

    print()
    if cosines:
        cmin = min(cosines)
        cmax = max(cosines)
        cmean = sum(cosines) / len(cosines)
        print(f"# summary: cosines over {len(cosines)} clips where BOTH paths produced embeddings")
        print(f"#   min={cmin:.6f}  mean={cmean:.6f}  max={cmax:.6f}")
    else:
        print("# summary: NO clips had embeddings from both paths")
    print(f"#   skipped-both={skipped_both}  one-sided-skip={one_sided}  mismatch={mismatches}")
    verdict = "PASS" if overall_pass else "FAIL"
    print(f"# overall: {verdict} (threshold cosine > {threshold})")

    return 0 if overall_pass else 1


if __name__ == "__main__":
    sys.exit(main())
