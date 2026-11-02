#!/usr/bin/env python3
"""Генерирует plan.json из {video,audio}.mp4 (make-media.sh).
Использование: make-plan.py <папка-с-медиа> <выходной plan.json>

Порядок сообщений зеркалит фикстуру two-source-buffers-mid-box-cuts:
generation_started, source_buffer_added x2, duration_changed, рамки данных
(номера с 0 по каждому SourceBuffer), end_of_stream.
Рамки режутся по произвольным смещениям (посреди боксов), как у YouTube.
"""
import json
import os
import sys


media_dir, plan_path = sys.argv[1], sys.argv[2]
sizes = {f: os.path.getsize(os.path.join(media_dir, f)) for f in ("video.mp4", "audio.mp4")}
seed = 12345
def rnd(lo, hi):
    global seed
    seed = (seed * 1103515245 + 12345) & 0x7fffffff
    return lo + seed % (hi - lo + 1)
DURATION_US = 20_000_000
tracks = [
    {"sb": 0, "file": "video.mp4", "mime": 'video/mp4; codecs="avc1.42c01f"', "lo": 6000, "hi": 90000},
    {"sb": 1, "file": "audio.mp4", "mime": 'audio/mp4; codecs="mp4a.40.2"', "lo": 3000, "hi": 30000},
]
steps = [{"text": {"type": "generation_started", "generation": 1}}]
for t in tracks:
    steps.append({"text": {"type": "source_buffer_added", "generation": 1, "source_buffer": t["sb"], "mime_type": t["mime"]}})
steps.append({"text": {"type": "duration_changed", "generation": 1, "duration": {"kind": "finite", "duration_us": DURATION_US}}})
pos = [0, 0]; seq = [0, 0]
while any(pos[t["sb"]] < sizes[t["file"]] for t in tracks):
    # берём дорожку с меньшей долей отправленного: видео и звук идут «по времени»
    open_tracks = [t for t in tracks if pos[t["sb"]] < sizes[t["file"]]]
    t = min(open_tracks, key=lambda t: pos[t["sb"]] / sizes[t["file"]])
    n = min(rnd(t["lo"], t["hi"]), sizes[t["file"]] - pos[t["sb"]])
    steps.append({"frame": {"generation": 1, "sb": t["sb"], "seq": seq[t["sb"]], "file": t["file"], "off": pos[t["sb"]], "len": n}})
    pos[t["sb"]] += n; seq[t["sb"]] += 1
steps.append({"text": {"type": "end_of_stream", "generation": 1}})
plan = {"files": list(sizes), "duration_us": DURATION_US, "expected_video_frames": 500, "steps": steps}
with open(plan_path, "w", encoding="utf-8") as plan_file:
    json.dump(plan, plan_file, separators=(",", ":"))
print("steps:", len(steps), "frames:", sum(1 for s in steps if "frame" in s), "video/audio frames:", seq)
