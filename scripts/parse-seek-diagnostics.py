#!/usr/bin/env python3
"""Лёгкий parser seek/scrub diagnostics из structured tracing logs."""

from __future__ import annotations

import argparse
import csv
import json
import re
import sys
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Iterable, TextIO


MARKERS = {
    "start": "Starting demux seek transaction",
    "accepted": "Demux seek transaction accepted",
    "packet": "Post-seek demux packet observed",
    "video_packet": "First post-seek video packet observed",
    "decoded": "First post-seek decoded frame observed",
    "presented": "First post-seek presented frame observed",
    "final_commit": "Final seek commit завершён",
    "stall": "Active seek transaction is still waiting",
    "tracks_changed": "Demuxer сообщил обновление track list",
    "rebase": "Active seek rebased after post-seek TracksChanged/ResetRequired marker",
    "audio_fallback": "Final seek commit продолжен через audio gate soft fallback",
    "preview_timeout": "Live scrub preview seek timed out",
}

DROP_NAMES = ("seek_preroll", "stale_generation", "late", "queue_overflow")
PLAYBACK_DROP_NAMES = ("late", "queue_overflow")
FAIL_ISSUES = {
    "tracks_changed_without_rebase",
    "normal_playback_drops_during_seek",
    "preview_timeout",
}

TIMESTAMP_RE = re.compile(r"^\s*(?P<ts>\d{4}-\d{2}-\d{2}[T ][0-9:.+-]+Z?)")
FIELD_RE = re.compile(r"(?P<key>[A-Za-z_][A-Za-z0-9_]*)=(?P<value>\"[^\"]*\"|[^,\s]+)")
ANSI_RE = re.compile(r"\x1b\[[0-9;]*m")
LONG_FRACTION_RE = re.compile(r"(\.\d{6})\d+")


@dataclass(frozen=True)
class Stamp:
    """Позиция события: timestamp из лога или line number."""

    line: int
    raw: str | None
    parsed: datetime | None

    def display(self) -> str:
        return self.raw if self.raw else f"line:{self.line}"


@dataclass
class Trace:
    """Одна строка acceptance matrix для одного seek transaction."""

    id: int
    source: str
    scenario: str
    kind: str = ""
    target_ms: str = ""
    actual_ms: str = ""
    generation: str = ""
    selected_video: str = ""
    selected_audio: str = ""
    start: Stamp | None = None
    accepted: Stamp | None = None
    first_packet: Stamp | None = None
    first_video_packet: Stamp | None = None
    first_decoded: Stamp | None = None
    first_presented: Stamp | None = None
    final_commit: Stamp | None = None
    preview_timeout: Stamp | None = None
    tracks_changed: int = 0
    rebase_count: int = 0
    audio_fallback: bool = False
    blockers: list[str] = field(default_factory=list)
    drops: dict[str, int] = field(default_factory=lambda: {name: 0 for name in DROP_NAMES})
    notes: list[str] = field(default_factory=list)

    def update(self, fields: dict[str, str]) -> None:
        """Заполняет stable fields из очередной structured строки."""

        self.kind = keep_first(self.kind, fields.get("kind"))
        self.target_ms = keep_first(self.target_ms, fields.get("target_ms"))
        self.actual_ms = keep_first(self.actual_ms, fields.get("actual_ms"))
        self.generation = keep_first(
            self.generation,
            fields.get("active_seek_generation")
            or fields.get("active_seek_generation_after")
            or fields.get("generation")
            or fields.get("pipeline_generation"),
        )
        self.selected_video = keep_first(self.selected_video, fields.get("selected_video_track_id"))
        self.selected_audio = keep_first(self.selected_audio, fields.get("selected_audio_track_id"))

    def open(self) -> bool:
        return self.final_commit is None and self.preview_timeout is None

    def preview_can_roll_forward(self) -> bool:
        """Preview completion сейчас не имеет отдельного log marker-а."""

        return self.kind == "preview" and self.first_presented is not None

    def note_blocker(self, fields: dict[str, str]) -> None:
        blocker = fields.get("blocker", "unknown")
        age_ms = fields.get("age_ms", "")
        audio_ready = fields.get("audio_gate_ready", "")
        age_suffix = f"@{age_ms}ms" if age_ms else ""
        audio_suffix = f",audio={audio_ready}" if audio_ready else ""
        self.blockers.append(f"{blocker}{age_suffix}{audio_suffix}")

    def latency_ms(self) -> str:
        if not self.start or not self.final_commit:
            return ""
        if not self.start.parsed or not self.final_commit.parsed:
            return ""
        delta = self.final_commit.parsed - self.start.parsed
        return f"{delta.total_seconds() * 1000:.1f}"

    def audio_gate(self) -> str:
        if self.audio_fallback:
            return "soft_fallback"
        if self.selected_audio == "None":
            return "no_selected_audio"
        audio_blockers = [blocker for blocker in self.blockers if blocker.startswith("audio_")]
        if audio_blockers:
            return audio_blockers[-1].split("@", maxsplit=1)[0]
        if self.final_commit:
            return "not_blocking"
        return "unknown"

    def issues(self) -> list[str]:
        issues: list[str] = []
        has_video = self.selected_video not in ("", "None")

        if not self.accepted:
            return ["missing_demux_accept"]
        if has_video and not self.first_packet:
            issues.append("missing_post_seek_packet")
        if has_video and not self.first_decoded:
            issues.append("missing_decoded_frame")
        if has_video and not self.first_presented:
            issues.append("missing_presented_frame")
        if self.kind == "final" and not self.final_commit:
            issues.append("missing_final_commit")
        if self.tracks_changed and not self.rebase_count:
            issues.append("tracks_changed_without_rebase")
        if sum(self.drops[name] for name in PLAYBACK_DROP_NAMES):
            issues.append("normal_playback_drops_during_seek")
        if self.drops["stale_generation"]:
            issues.append("stale_generation_discards")
        if self.audio_fallback:
            issues.append("audio_soft_fallback")
        if self.blockers:
            issues.append("blocker_over_250ms")
        if self.preview_timeout:
            issues.append("preview_timeout")

        return issues

    def status(self) -> str:
        issues = self.issues()
        if any(issue.startswith("missing_") or issue in FAIL_ISSUES for issue in issues):
            return "FAIL"
        return "WARN" if issues else "OK"

    def row(self) -> dict[str, str]:
        issues = self.issues()
        return {
            "id": str(self.id),
            "source": self.source,
            "scenario": self.scenario,
            "kind": self.kind,
            "target_ms": self.target_ms,
            "actual_ms": self.actual_ms,
            "generation": self.generation,
            "start": stamp_text(self.start),
            "demux_accepted": stamp_text(self.accepted),
            "first_post_seek_packet": stamp_text(self.first_packet),
            "first_video_packet": stamp_text(self.first_video_packet),
            "first_decoded_frame": stamp_text(self.first_decoded),
            "first_presented_frame": stamp_text(self.first_presented),
            "final_commit": stamp_text(self.final_commit),
            "commit_latency_ms": self.latency_ms(),
            "blocker_gt_250ms": self.blockers[-1] if self.blockers else "",
            "drops_seek_stale_late_queue": "/".join(str(self.drops[name]) for name in DROP_NAMES),
            "audio_gate": self.audio_gate(),
            "status": self.status(),
            "notes": ",".join(self.notes + issues),
        }


def keep_first(current: str, incoming: str | None) -> str:
    if current:
        return current
    return clean_value(incoming) if incoming else ""


def clean_value(value: str | None) -> str:
    if value is None:
        return ""
    cleaned = value.strip().strip(",")
    if len(cleaned) >= 2 and cleaned[0] == '"' and cleaned[-1] == '"':
        return cleaned[1:-1]
    return cleaned


def stamp_text(stamp: Stamp | None) -> str:
    return stamp.display() if stamp else ""


def parse_stamp(line: str, line_number: int) -> Stamp:
    match = TIMESTAMP_RE.match(line)
    raw = match.group("ts") if match else None
    return Stamp(line=line_number, raw=raw, parsed=parse_datetime(raw))


def parse_datetime(raw: str | None) -> datetime | None:
    if not raw:
        return None
    normalized = LONG_FRACTION_RE.sub(r"\1", raw.replace("Z", "+00:00"))
    try:
        return datetime.fromisoformat(normalized)
    except ValueError:
        return None


def parse_fields(line: str) -> dict[str, str]:
    return {match.group("key"): clean_value(match.group("value")) for match in FIELD_RE.finditer(line)}


def classify_marker(line: str) -> str | None:
    for marker_name, marker_text in MARKERS.items():
        if marker_text in line:
            return marker_name
    return None


def classify_drop(line: str, fields: dict[str, str]) -> str | None:
    reason = fields.get("reason", "")
    if reason.endswith("SeekPreroll"):
        return "seek_preroll"
    if reason.endswith("StaleGeneration"):
        return "stale_generation"
    if reason.endswith("Late"):
        return "late"
    if reason.endswith("QueueOverflow"):
        return "queue_overflow"

    if "Dropping decoded video frame from stale seek generation" in line:
        return "stale_generation"
    if "Dropping video packet until decoder receives post-flush keyframe" in line:
        return "seek_preroll"
    if "Dropping oldest queued video frame before enqueue" in line:
        return "queue_overflow"
    if "Dropping frame: queue overflow protection" in line:
        return "queue_overflow"
    return None


def parse_logs(paths: Iterable[Path], scenario: str) -> list[Trace]:
    traces: list[Trace] = []
    active: Trace | None = None

    for path in paths:
        with open_log(path) as log_file:
            for line_number, raw_line in enumerate(log_file, start=1):
                line = ANSI_RE.sub("", raw_line.rstrip("\n"))
                fields = parse_fields(line)
                stamp = parse_stamp(line, line_number)
                drop_name = classify_drop(line, fields)

                if active and active.open() and drop_name:
                    active.drops[drop_name] += 1
                    continue

                marker = classify_marker(line)
                if marker == "start":
                    if active and active.open() and not active.preview_can_roll_forward():
                        active.notes.append("new_seek_started_before_previous_commit")
                    active = Trace(id=len(traces) + 1, source=str(path), scenario=scenario)
                    active.start = stamp
                    active.update(fields)
                    traces.append(active)
                    continue

                if active is None or marker is None:
                    continue

                active.update(fields)
                if marker == "accepted":
                    active.accepted = active.accepted or stamp
                elif marker == "packet":
                    active.first_packet = active.first_packet or stamp
                elif marker == "video_packet":
                    active.first_packet = active.first_packet or stamp
                    active.first_video_packet = active.first_video_packet or stamp
                elif marker == "decoded":
                    active.first_decoded = active.first_decoded or stamp
                elif marker == "presented":
                    active.first_presented = active.first_presented or stamp
                elif marker == "final_commit":
                    active.final_commit = active.final_commit or stamp
                    active = None
                elif marker == "preview_timeout":
                    active.preview_timeout = stamp
                    active = None
                elif marker == "stall":
                    active.note_blocker(fields)
                elif marker == "tracks_changed":
                    active.tracks_changed += 1
                elif marker == "rebase":
                    active.rebase_count += 1
                elif marker == "audio_fallback":
                    active.audio_fallback = True

    return traces


def open_log(path: Path) -> TextIO:
    if str(path) == "-":
        return sys.stdin
    return path.open("r", encoding="utf-8", errors="replace")


def print_markdown(rows: list[dict[str, str]]) -> None:
    columns = [
        "id",
        "scenario",
        "kind",
        "target_ms",
        "start",
        "demux_accepted",
        "first_post_seek_packet",
        "first_decoded_frame",
        "first_presented_frame",
        "final_commit",
        "blocker_gt_250ms",
        "drops_seek_stale_late_queue",
        "audio_gate",
        "status",
        "notes",
    ]
    print("| " + " | ".join(columns) + " |")
    print("| " + " | ".join("---" for _ in columns) + " |")
    for row in rows:
        print("| " + " | ".join(row[column].replace("|", "\\|") for column in columns) + " |")


def print_csv(rows: list[dict[str, str]]) -> None:
    if not rows:
        return
    writer = csv.DictWriter(sys.stdout, fieldnames=list(rows[0].keys()))
    writer.writeheader()
    writer.writerows(rows)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="Parse rustiplayer seek diagnostics logs into an acceptance table.",
    )
    parser.add_argument("logs", nargs="+", type=Path, help="Log files. Use '-' for stdin.")
    parser.add_argument("--scenario", default="manual", help="Label added to each parsed row.")
    parser.add_argument(
        "--format",
        choices=("markdown", "csv", "json"),
        default="markdown",
        help="Output format.",
    )
    return parser.parse_args()


def main() -> int:
    args = parse_args()
    rows = [trace.row() for trace in parse_logs(args.logs, args.scenario)]

    if args.format == "csv":
        print_csv(rows)
    elif args.format == "json":
        json.dump(rows, sys.stdout, ensure_ascii=False, indent=2)
        print()
    else:
        print_markdown(rows)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
