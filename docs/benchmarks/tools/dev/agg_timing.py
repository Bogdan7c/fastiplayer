#!/usr/bin/env python3
"""Агрегирует строки trace `render frame stage timings` из лога плеера: mean/p50/p90/max по стадиям."""

import re
import sys

ANSI = re.compile(r"\x1b\[[0-9;]*m")
FIELD = re.compile(r"([a-z_]+_ms)=([0-9.]+)")


def percentile(values, fraction):
    ordered = sorted(values)
    index = min(len(ordered) - 1, max(0, int(round(fraction * (len(ordered) - 1)))))
    return ordered[index]


def main(path, skip_first=120):
    rows = []
    with open(path, errors="replace") as handle:
        for line in handle:
            if "render frame stage timings" not in line:
                continue
            clean = ANSI.sub("", line)
            rows.append({name: float(value) for name, value in FIELD.findall(clean)})
    rows = rows[skip_first:]
    if not rows:
        print("no timing rows")
        return
    print(f"{path}: {len(rows)} frames after warmup skip")
    names = [name for name in rows[0] if name != "frame_budget_ms"]
    print(f"{'stage':<22}{'mean':>8}{'p50':>8}{'p90':>8}{'max':>8}{'sum/s(ms)':>11}")
    for name in names:
        values = [row.get(name, 0.0) for row in rows]
        mean = sum(values) / len(values)
        print(f"{name:<22}{mean:8.3f}{percentile(values, 0.5):8.3f}{percentile(values, 0.9):8.3f}{max(values):8.3f}")
    budget = rows[0].get("frame_budget_ms", 0)
    over = sum(1 for row in rows if row["frame_total_ms"] > budget) if budget else 0
    print(f"frame_budget_ms={budget}; frames over budget: {over}")
    print("(frame_total — wall-время главного потока на кадр; mean*60 ≈ ms CPU-занятости в секунду при 60 Гц, если нет ожиданий)")


if __name__ == "__main__":
    for path in sys.argv[1:]:
        main(path)
        print()
