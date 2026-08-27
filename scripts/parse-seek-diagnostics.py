#!/usr/bin/env python3
"""Разбор structured seek diagnostics логов rustiplayer.

Parser работает только с опубликованными final-seek markers. Он не принимает
playback-решений и не читает runtime state напрямую: входом являются обычные
stderr/stdout строки, которые пишет `player-core`.
"""

from __future__ import annotations

import argparse
import csv
import json
import re
import sys
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable, TextIO

from seek_diagnostics_metrics import SUMMARY_FIELDNAMES, SeekTimingMetrics, build_summary
from seek_diagnostics_metrics import format_milliseconds, write_summary


START_MARKER = "Starting demux seek transaction"
WORKER_REQUEST_MARKER = "Prepared demux seek request enqueued"
WORKER_RECEIPT_MARKER = "Prepared demux seek receipt accepted"
ACCEPTED_MARKER = "Demux seek transaction accepted"
PACKET_MARKER = "Post-seek demux packet observed"
VIDEO_PACKET_MARKER = "First post-seek video packet observed"
DECODED_MARKER = "First post-seek decoded frame observed"
PRESENTED_MARKER = "First post-seek presented frame observed"
COMMIT_MARKER = "Final seek commit завершён"
COMMIT_TIMEOUT_MARKER = "Final seek commit остановлен по timeout"
WAITING_MARKER = "Active seek transaction is still waiting"
TRACKS_CHANGED_MARKER = "Demuxer сообщил обновление track list"
REBASED_MARKER = "Active seek rebased after post-seek TracksChanged/ResetRequired marker"
AUDIO_SOFT_FALLBACK_MARKER = "Final seek commit продолжен через audio gate soft fallback"
AUDIO_PLAY_ACCEPTED_MARKER = "Audio play accepted before final seek commit"
HTTP_HEADERS_READY_MARKERS = (
    "Source HTTP response headers ready",
    "Adaptive streaming HTTP headers ready",
)
HTTP_FIRST_BODY_READY_MARKERS = (
    "Source HTTP first non-empty body chunk ready",
    "Adaptive streaming HTTP first body chunk ready",
)
HTTP_BODY_COMPLETE_MARKER = "Source HTTP validated body complete"
MANIFEST_CANDIDATE_MARKER = "HLS manifest seek candidate"
MANIFEST_ANCHOR_PROVEN_MARKER = "HLS manifest seek anchor proven"

FOLLOWUP_MARKERS = (
    ACCEPTED_MARKER,
    PACKET_MARKER,
    VIDEO_PACKET_MARKER,
    DECODED_MARKER,
    PRESENTED_MARKER,
    COMMIT_MARKER,
    COMMIT_TIMEOUT_MARKER,
    WAITING_MARKER,
    TRACKS_CHANGED_MARKER,
    REBASED_MARKER,
    AUDIO_SOFT_FALLBACK_MARKER,
    WORKER_RECEIPT_MARKER,
    AUDIO_PLAY_ACCEPTED_MARKER,
    *HTTP_HEADERS_READY_MARKERS,
    *HTTP_FIRST_BODY_READY_MARKERS,
    HTTP_BODY_COMPLETE_MARKER,
    MANIFEST_CANDIDATE_MARKER,
    MANIFEST_ANCHOR_PROVEN_MARKER,
)

BLOCKER_WARN_MS = 250.0
UNKNOWN_SOURCE = "<stdin>"

FIELDNAMES = [
    "source",
    "scenario",
    "generation",
    "kind",
    "target_ms",
    "actual_ms",
    "committed_ms",
    "seek_start",
    "demux_accepted",
    "first_packet",
    "first_decoded",
    "first_presented",
    "worker_round_trip_ms",
    "network_first_byte_ms",
    "network_body_complete_ms",
    "receipt_to_presented_ms",
    "receipt_to_audio_ms",
    "public_to_presented_ms",
    "public_to_audio_ms",
    "final_commit",
    "blocker_over_250ms",
    "drops_seek_stale_late_queue",
    "audio_gate",
    "verdict",
]


@dataclass
class SeekTransaction:
    """Одна final seek transaction, собранная из diagnostics markers."""

    sequence: int
    source: str
    scenario: str
    generation: str
    start_line: int
    kind: str = "seek"
    target_ms: str = ""
    actual_ms: str = ""
    committed_ms: str = ""
    selected_video_track_id: str = ""
    selected_audio_track_id: str = ""
    demux_started: bool = False
    demux_accepted: bool = False
    first_packet: bool = False
    first_video_packet: bool = False
    first_decoded: bool = False
    first_presented: bool = False
    final_commit: bool = False
    commit_timed_out: bool = False
    tracks_changed: bool = False
    rebased_after_tracks_changed: bool = False
    blocker: str = ""
    blocker_age_ms: float = 0.0
    drops_seek_preroll: int = 0
    drops_stale_generation: int = 0
    drops_late: int = 0
    drops_queue_overflow: int = 0
    audio_gate: str = ""
    superseded: bool = False
    timing: SeekTimingMetrics = field(default_factory=SeekTimingMetrics)

    def observe_common_fields(self, line: str) -> None:
        """Обновляет общие поля, которые встречаются на разных markers."""

        self.kind = field_value(line, "kind") or self.kind
        self.target_ms = (
            field_value(line, "target_ms")
            or field_value(line, "target_milliseconds")
            or self.target_ms
        )
        self.actual_ms = (
            field_value(line, "actual_ms")
            or field_value(line, "actual_milliseconds")
            or self.actual_ms
        )
        self.committed_ms = field_value(line, "committed_ms") or self.committed_ms
        self.selected_video_track_id = (
            field_value(line, "selected_video_track_id") or self.selected_video_track_id
        )
        self.selected_audio_track_id = (
            field_value(line, "selected_audio_track_id") or self.selected_audio_track_id
        )

    def observe_waiting_marker(self, line: str) -> None:
        """Сохраняет последний blocker active seek-а и максимальный возраст."""

        self.blocker = field_value(line, "blocker") or self.blocker
        self.blocker_age_ms = max(self.blocker_age_ms, float_field(line, "age_ms") or 0.0)
        stale_discards = int_field(line, "stale_generation_discards")
        if stale_discards is not None:
            self.drops_stale_generation = max(self.drops_stale_generation, stale_discards)

    def observe_drop_line(self, line: str) -> None:
        """Учитывает drop/discard строки, если они появились во время active seek."""

        reason = drop_reason(line)
        if reason == "seek_preroll":
            self.drops_seek_preroll += 1
        elif reason == "stale_generation":
            self.drops_stale_generation += 1
        elif reason == "late":
            self.drops_late += 1
        elif reason == "queue_overflow":
            self.drops_queue_overflow += 1

    def video_required(self, media_kind: str) -> bool:
        """Определяет, нужно ли требовать decoded/presented video markers."""

        if media_kind == "audio":
            return False
        if media_kind == "video":
            return True
        if self.selected_video_track_id:
            return not none_like(self.selected_video_track_id)
        return self.first_video_packet or self.first_decoded or self.first_presented

    def verdict(self, media_kind: str) -> str:
        """Возвращает PASS/WARN/FAIL без preview-only условий."""

        if self.drops_late > 0 or self.drops_queue_overflow > 0:
            return "FAIL"
        if self.missing_required_markers(media_kind):
            return "FAIL"
        if self.warning_conditions():
            return "WARN"
        return "PASS"

    def missing_required_markers(self, media_kind: str) -> list[str]:
        """Перечисляет отсутствующие final-seek markers для текущего media kind."""

        missing = []
        if not self.demux_accepted:
            missing.append("demux_accepted")
        if not self.first_packet:
            missing.append("first_packet")
        if self.video_required(media_kind) and not self.first_decoded:
            missing.append("first_decoded")
        if self.video_required(media_kind) and not self.first_presented:
            missing.append("first_presented")
        if self.timing.worker_receipted and self.timing.worker_round_trip_ms is None:
            missing.append("worker_receipt")
        if (
            self.timing.worker_receipted
            and self.video_required(media_kind)
            and self.timing.receipt_to_presented_ms is None
        ):
            missing.append("receipt_to_presented")
        audio_required = not none_like(self.selected_audio_track_id)
        if audio_required and self.timing.receipt_to_audio_ms is None:
            missing.append("audio_play_accepted")
        if not self.final_commit:
            missing.append("final_commit")
        return missing

    def warning_conditions(self) -> bool:
        """Отделяет успешный commit с подозрительной диагностикой от чистого PASS."""

        return (
            self.drops_stale_generation > 0
            or self.audio_gate != ""
            or self.blocker_age_ms > BLOCKER_WARN_MS
            or (self.tracks_changed and not self.rebased_after_tracks_changed)
        )

    def to_row(self, media_kind: str) -> dict[str, str]:
        """Преобразует transaction в стабильный CSV/table contract."""

        return {
            "source": self.source,
            "scenario": self.scenario,
            "generation": self.generation,
            "kind": self.kind,
            "target_ms": self.target_ms,
            "actual_ms": self.actual_ms,
            "committed_ms": self.committed_ms,
            "seek_start": yes_no(self.demux_started),
            "demux_accepted": yes_no(self.demux_accepted),
            "first_packet": yes_no(self.first_packet),
            "first_decoded": yes_no(self.first_decoded),
            "first_presented": yes_no(self.first_presented),
            "worker_round_trip_ms": format_milliseconds(
                self.timing.worker_round_trip_ms
            ),
            "network_first_byte_ms": format_milliseconds(
                self.timing.network_first_byte_ms()
            ),
            "network_body_complete_ms": format_milliseconds(
                self.timing.network_body_complete_ms
            ),
            "receipt_to_presented_ms": format_milliseconds(
                self.timing.receipt_to_presented_ms
            ),
            "receipt_to_audio_ms": format_milliseconds(
                self.timing.receipt_to_audio_ms
            ),
            "public_to_presented_ms": format_milliseconds(
                self.timing.public_to_presented_ms()
            ),
            "public_to_audio_ms": format_milliseconds(
                self.timing.public_to_audio_ms()
            ),
            "final_commit": yes_no(self.final_commit),
            "blocker_over_250ms": self.blocker_cell(),
            "drops_seek_stale_late_queue": self.drop_cell(),
            "audio_gate": self.audio_gate,
            "verdict": self.verdict(media_kind),
        }

    def blocker_cell(self) -> str:
        """Печатает blocker только когда он важен для acceptance."""

        if not self.blocker:
            return ""
        if self.blocker_age_ms <= BLOCKER_WARN_MS:
            return ""
        return f"{self.blocker}@{self.blocker_age_ms:.0f}ms"

    def drop_cell(self) -> str:
        """Сохраняет порядок columns из acceptance таблицы."""

        return (
            f"{self.drops_seek_preroll}/"
            f"{self.drops_stale_generation}/"
            f"{self.drops_late}/"
            f"{self.drops_queue_overflow}"
        )


class SeekDiagnosticsParser:
    """Stateful parser, который связывает markers в seek transactions."""

    def __init__(self, scenario: str, media_kind: str) -> None:
        self.scenario = scenario
        self.media_kind = media_kind
        self.transactions: list[SeekTransaction] = []
        self.active_by_generation: dict[str, SeekTransaction] = {}
        self.last_active: SeekTransaction | None = None

    def parse_path(self, path: Path) -> None:
        """Читает один файл или stdin."""

        if str(path) == "-":
            self.parse_lines(sys.stdin, UNKNOWN_SOURCE)
            return
        with path.open("r", encoding="utf-8", errors="replace") as log_file:
            self.parse_lines(log_file, str(path))

    def parse_lines(self, lines: Iterable[str], source: str) -> None:
        """Обрабатывает строки без предположений о формате timestamp-а."""

        for line_number, line in enumerate(lines, start=1):
            self.parse_line(source, line_number, line.rstrip("\n"))

    def parse_line(self, source: str, line_number: int, line: str) -> None:
        """Маршрутизирует строку к marker-specific обработчику."""

        if WORKER_REQUEST_MARKER in line:
            self.start_transaction(source, line_number, line, worker_receipted=True)
            return
        if START_MARKER in line:
            self.start_transaction(source, line_number, line)
            return

        if not self.is_relevant_followup_line(line):
            return

        receipt_generation = generation_from_line(line)
        if WORKER_RECEIPT_MARKER in line and receipt_generation is not None:
            transaction = self.active_by_generation.get(receipt_generation)
        else:
            transaction = self.transaction_for_line(source, line_number, line)
        if transaction is None:
            return

        transaction.observe_common_fields(line)

        if WORKER_RECEIPT_MARKER in line:
            transaction.timing.worker_receipted = True
            transaction.timing.worker_round_trip_ms = float_field(
                line, "elapsed_milliseconds"
            )
        elif any(marker in line for marker in HTTP_HEADERS_READY_MARKERS):
            if transaction.timing.network_headers_ms is None:
                transaction.timing.network_headers_ms = float_field(
                    line, "elapsed_milliseconds"
                )
        elif any(marker in line for marker in HTTP_FIRST_BODY_READY_MARKERS):
            if transaction.timing.network_first_body_ms is None:
                transaction.timing.network_first_body_ms = float_field(
                    line, "elapsed_milliseconds"
                )
        elif HTTP_BODY_COMPLETE_MARKER in line:
            if transaction.timing.network_body_complete_ms is None:
                transaction.timing.network_body_complete_ms = float_field(
                    line, "elapsed_milliseconds"
                )
        elif MANIFEST_ANCHOR_PROVEN_MARKER in line:
            transaction.timing.manifest_anchor_proven = True
        elif MANIFEST_CANDIDATE_MARKER in line:
            if "HLS manifest seek candidate started" in line:
                transaction.timing.manifest_candidate_count += 1
            if "HLS manifest seek candidate accepted" in line:
                transaction.timing.manifest_candidate_accepted = True
            transaction.timing.manifest_candidate_elapsed_ms = max(
                transaction.timing.manifest_candidate_elapsed_ms,
                float_field(line, "elapsed_milliseconds") or 0.0,
            )
        elif ACCEPTED_MARKER in line:
            transaction.demux_accepted = True
        elif VIDEO_PACKET_MARKER in line:
            transaction.first_packet = True
            transaction.first_video_packet = True
        elif PACKET_MARKER in line:
            transaction.first_packet = True
        elif DECODED_MARKER in line:
            transaction.first_decoded = True
        elif PRESENTED_MARKER in line:
            transaction.first_presented = True
            elapsed_ms = float_field(line, "elapsed_ms")
            if elapsed_ms is not None:
                transaction.timing.receipt_to_presented_ms = elapsed_ms
        elif AUDIO_PLAY_ACCEPTED_MARKER in line:
            accepted_after_ms = float_field(line, "accepted_after_ms")
            if accepted_after_ms is not None:
                transaction.timing.receipt_to_audio_ms = accepted_after_ms
        elif COMMIT_MARKER in line:
            transaction.final_commit = True
            self.close_transaction(transaction)
        elif COMMIT_TIMEOUT_MARKER in line:
            transaction.commit_timed_out = True
        elif WAITING_MARKER in line:
            transaction.observe_waiting_marker(line)
        elif TRACKS_CHANGED_MARKER in line:
            transaction.tracks_changed = True
        elif REBASED_MARKER in line:
            transaction.rebased_after_tracks_changed = True
        elif AUDIO_SOFT_FALLBACK_MARKER in line:
            transaction.audio_gate = "soft_fallback"
        else:
            transaction.observe_drop_line(line)

    def is_relevant_followup_line(self, line: str) -> bool:
        """Не даёт unrelated `generation=...` строкам создавать fake seek rows."""

        if any(marker in line for marker in FOLLOWUP_MARKERS):
            return True
        return self.last_active is not None and drop_reason(line) != ""

    def start_transaction(
        self,
        source: str,
        line_number: int,
        line: str,
        *,
        worker_receipted: bool = False,
    ) -> None:
        """Создаёт transaction на marker-е начала demux seek-а."""

        if (
            self.last_active is not None
            and self.last_active.source == source
            and not self.last_active.final_commit
        ):
            self.last_active.superseded = True
            self.active_by_generation.pop(self.last_active.generation, None)
        generation = generation_from_line(line) or synthetic_generation(source, line_number)
        transaction = SeekTransaction(
            sequence=len(self.transactions) + 1,
            source=source,
            scenario=self.scenario,
            generation=generation,
            start_line=line_number,
            demux_started=True,
            timing=SeekTimingMetrics(worker_receipted=worker_receipted),
        )
        transaction.observe_common_fields(line)
        self.transactions.append(transaction)
        self.active_by_generation[generation] = transaction
        self.last_active = transaction

    def transaction_for_line(
        self,
        source: str,
        line_number: int,
        line: str,
    ) -> SeekTransaction | None:
        """Находит active transaction по generation или последнему seek-у."""

        generation = generation_from_line(line)
        if generation is not None and generation in self.active_by_generation:
            return self.active_by_generation[generation]
        if self.last_active is not None and not self.last_active.final_commit:
            return self.last_active
        if generation is None:
            return None

        transaction = SeekTransaction(
            sequence=len(self.transactions) + 1,
            source=source,
            scenario=self.scenario,
            generation=generation,
            start_line=line_number,
        )
        transaction.observe_common_fields(line)
        self.transactions.append(transaction)
        self.active_by_generation[generation] = transaction
        self.last_active = transaction
        return transaction

    def close_transaction(self, transaction: SeekTransaction) -> None:
        """Снимает закрытый final seek из active map."""

        self.active_by_generation.pop(transaction.generation, None)
        if self.last_active is transaction:
            self.last_active = None

    def rows(self) -> list[dict[str, str]]:
        """Возвращает строки результата в порядке появления seek-ов."""

        return [transaction.to_row(self.media_kind) for transaction in self.transactions]

    def summary_rows(self) -> list[dict[str, str]]:
        """Считает nearest-rank latency summary только по completed success."""

        return build_summary(
            transaction.timing
            for transaction in self.transactions
            if not transaction.superseded
            and transaction.final_commit
            and transaction.verdict(self.media_kind) != "FAIL"
        )


def parse_args(argv: list[str]) -> argparse.Namespace:
    """Создаёт CLI без внешних зависимостей."""

    parser = argparse.ArgumentParser(
        description="Parse rustiplayer final seek diagnostics logs.",
    )
    parser.add_argument(
        "logs",
        nargs="+",
        type=Path,
        help="Log files to parse; use '-' for stdin.",
    )
    parser.add_argument(
        "--scenario",
        default="",
        help="Scenario label copied into each output row.",
    )
    parser.add_argument(
        "--format",
        choices=("table", "csv", "json"),
        default="table",
        help="Output format.",
    )
    parser.add_argument(
        "--media-kind",
        choices=("auto", "video", "audio"),
        default="auto",
        help="Controls whether decoded/presented video markers are required.",
    )
    parser.add_argument(
        "--strict",
        action="store_true",
        help="Exit with status 1 when any parsed transaction has FAIL verdict.",
    )
    parser.add_argument(
        "--summary",
        action="store_true",
        help=(
            "Output summary instead of rows; p50/p95 use nearest-rank "
            "ceil(p*n)-1 over completed successful transactions."
        ),
    )
    return parser.parse_args(argv)


def generation_from_line(line: str) -> str | None:
    """Возвращает seek generation из разных diagnostics field names."""

    for key in (
        "generation",
        "active_seek_generation",
        "active_seek_generation_after",
        "active_seek_generation_before",
    ):
        value = field_value(line, key)
        if value is not None:
            return value
    return None


def field_value(line: str, key: str) -> str | None:
    """Достаёт `key=value` из tracing-style structured log строки."""

    pattern = rf"\b{re.escape(key)}\s*=\s*(\"[^\"]*\"|'[^']*'|[^\s,]+)"
    match = re.search(pattern, line)
    if match is None:
        return None
    return clean_value(match.group(1))


def clean_value(raw_value: str) -> str:
    """Нормализует value без попытки понять его Rust type."""

    value = raw_value.strip().strip(",")
    if len(value) >= 2 and value[0] == value[-1] and value[0] in {"'", '"'}:
        value = value[1:-1]
    return value


def float_field(line: str, key: str) -> float | None:
    """Парсит числовой field, если он есть."""

    value = field_value(line, key)
    if value is None:
        return None
    try:
        return float(value)
    except ValueError:
        return None


def int_field(line: str, key: str) -> int | None:
    """Парсит integer field, если он есть."""

    value = field_value(line, key)
    if value is None:
        return None
    try:
        return int(float(value))
    except ValueError:
        return None


def drop_reason(line: str) -> str:
    """Классифицирует drop reason только по явным markers/fields."""

    lowered = line.lower()
    if not any(word in lowered for word in ("drop", "dropped", "discard")):
        return ""

    explicit_reason = (
        field_value(line, "reason")
        or field_value(line, "drop_reason")
        or field_value(line, "video_drop_reason")
        or ""
    ).lower()
    reason_text = f"{explicit_reason} {lowered}"

    if "seek_preroll" in reason_text or "seekpreroll" in reason_text:
        return "seek_preroll"
    if "stale_generation" in reason_text or "stalegeneration" in reason_text:
        return "stale_generation"
    if "queue_overflow" in reason_text or "queueoverflow" in reason_text:
        return "queue_overflow"
    if explicit_reason == "late" or "videodropreason::late" in reason_text:
        return "late"
    return ""


def none_like(value: str) -> bool:
    """Распознаёт Rust Option::None в compact debug output."""

    return value in {"None", "null", "nil", ""}


def synthetic_generation(source: str, line_number: int) -> str:
    """Создаёт стабильный id для неполного/неструктурированного лога."""

    return f"{source}:{line_number}"


def yes_no(value: bool) -> str:
    """Печатает bool коротко и стабильно."""

    return "yes" if value else "no"


def write_csv(rows: list[dict[str, str]], output: TextIO) -> None:
    """Пишет CSV contract для spreadsheets и diff-а."""

    writer = csv.DictWriter(output, fieldnames=FIELDNAMES)
    writer.writeheader()
    writer.writerows(rows)


def write_json(rows: list[dict[str, str]], output: TextIO) -> None:
    """Пишет JSON без preview-specific columns."""

    json.dump(rows, output, ensure_ascii=False, indent=2)
    output.write("\n")


def write_table(rows: list[dict[str, str]], output: TextIO) -> None:
    """Пишет compact text table для ручного smoke-а."""

    if not rows:
        output.write("No seek transactions found.\n")
        return

    widths = {
        field: max(len(field), *(len(row[field]) for row in rows))
        for field in FIELDNAMES
    }
    header = " | ".join(field.ljust(widths[field]) for field in FIELDNAMES)
    separator = "-+-".join("-" * widths[field] for field in FIELDNAMES)
    output.write(f"{header}\n")
    output.write(f"{separator}\n")
    for row in rows:
        output.write(" | ".join(row[field].ljust(widths[field]) for field in FIELDNAMES))
        output.write("\n")


def main(argv: list[str]) -> int:
    """Точка входа CLI."""

    args = parse_args(argv)
    parser = SeekDiagnosticsParser(scenario=args.scenario, media_kind=args.media_kind)
    for path in args.logs:
        parser.parse_path(path)

    rows = parser.rows()
    if args.summary:
        write_summary(parser.summary_rows(), args.format, sys.stdout)
    elif args.format == "csv":
        write_csv(rows, sys.stdout)
    elif args.format == "json":
        write_json(rows, sys.stdout)
    else:
        write_table(rows, sys.stdout)

    if args.strict and any(row["verdict"] == "FAIL" for row in rows):
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
