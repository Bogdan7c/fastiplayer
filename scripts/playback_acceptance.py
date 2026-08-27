"""Deterministic analyzer критического пути startup/seek из tracing logs.

Модуль ничего не запускает и не меняет пользовательский профиль. Он принимает
уже собранные строки лога, связывает owner-local telemetry и отделяет три исхода:
доказанный успех, явную ошибку и неполный sample с отсутствующими markers.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from pathlib import Path
PROCESS_START_MARKERS = ("=== rustiplayer ===", "Запуск приложения")
STARTUP_MEDIA_OPEN_ACCEPTED_MARKER = "Startup media-open/restore accepted"
MEDIA_OPEN_ACCEPTED_MARKERS = (
    STARTUP_MEDIA_OPEN_ACCEPTED_MARKER,
    "Startup restore Installed",
    "Startup media Installed",
    "Startup direct media Installed",
)
STARTUP_PRESENTED_MARKER = "First startup video frame presented"
STARTUP_AUDIO_OUTPUT_READY_MARKER = "Startup audio output ready"
STARTUP_AUDIO_RESUMED_MARKER = "Startup audio playback resumed"
STARTUP_AUDIO_NOT_REQUIRED_MARKER = "Startup audio gate not required"
STARTUP_READY_MARKER = "Startup presentation and audio gates ready"
WORKER_REQUEST_MARKER = "Prepared demux seek request enqueued"
WORKER_RECEIPT_MARKER = "Prepared demux seek receipt accepted"
DEMUX_ACCEPTED_MARKER = "Demux seek transaction accepted"
PRESENTED_MARKER = "First post-seek presented frame observed"
DECODED_MARKER = "First post-seek decoded frame observed"
AUDIO_ACCEPTED_MARKER = "Audio play accepted before final seek commit"
COMMIT_MARKER = "Final seek commit завершён"
COMMIT_TIMEOUT_MARKER = "Final seek commit остановлен по timeout"
WAITING_MARKER = "Active seek transaction is still waiting"
HTTP_REQUEST_MARKER = "Source HTTP request started"
HTTP_HEADERS_MARKER = "Source HTTP response headers ready"
HTTP_FIRST_BODY_MARKER = "Source HTTP first non-empty body chunk ready"
HTTP_BODY_COMPLETE_MARKER = "Source HTTP validated body complete"
HTTP_BOUNDED_TERMINAL_MARKER = "Bounded HTTP request terminal"
HTTP_CANCELLED_MARKERS = (
    "Source HTTP request cancelled",
    "Source HTTP request canceled",
)
POSITION_PROGRESS_MARKERS = (
    "Post-seek position progress observed",
    "Post-seek position advanced",
    "Seek position progress observed",
)
PRE_TARGET_PRESENTED_MARKERS = (
    "Pre-target frame presented",
    "Visible pre-target frame observed",
)
SEEK_FAILURE_MARKERS = (
    "SeekUnavailable",
    "Prepared demux seek failed",
    "Prepared demux seek worker has stopped",
    "Audio play failed before final seek commit",
)
ROLLBACK_MARKERS = (
    "Seek position rolled back",
    "Seek rollback to zero",
)

TIMESTAMP_PATTERN = re.compile(
    r"^(?P<timestamp>\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2}))"
)


class Verdict(str, Enum):
    """Acceptance outcome без маскировки отсутствующей telemetry."""

    PASS = "PASS"
    FAIL = "FAIL"
    INCOMPLETE = "INCOMPLETE"
    SUPERSEDED = "SUPERSEDED"


class StartupPlaybackExpectation(str, Enum):
    """Typed playback gate из app-owned startup marker-а."""

    PLAYING = "Playing"
    PAUSED = "Paused"


class StartupAudioExpectation(str, Enum):
    """Typed audio gate из app-owned startup marker-а."""

    REQUIRED = "Required"
    NOT_PRESENT = "NotPresent"
    UNKNOWN = "Unknown"


@dataclass(frozen=True)
class LogPoint:
    """Позиция marker-а внутри одного process log."""

    source: str
    line_number: int
    wall_timestamp_ms: float | None
    process_elapsed_ms: float | None


@dataclass
class ScrubTimeline:
    """Begin/Preview/End stages одного timeline drag."""

    begin: LogPoint | None = None
    previews: list[LogPoint] = field(default_factory=list)
    end: LogPoint | None = None
    begin_to_first_preview_ms: float | None = None
    begin_to_end_ms: float | None = None
    correlation_failures: list[str] = field(default_factory=list)


@dataclass
class NetworkRequest:
    """Один bounded HTTP stream без хранения URL или других secrets."""

    sequence: int
    source: str
    safe_request_id: str
    operation_kind: str
    owner_seek_sequence: int | None
    started: LogPoint
    headers_ms: float | None = None
    first_body_ms: float | None = None
    body_complete_ms: float | None = None
    body_bytes: int | None = None
    body_complete_point: LogPoint | None = None
    cancelled_ms: float | None = None
    cancelled_point: LogPoint | None = None
    terminal_outcome: str = ""
    terminal_ms: float | None = None
    terminal_point: LogPoint | None = None
    terminal_error_category: str = ""
    ambiguous: bool = False

    def missing_stages(self) -> list[str]:
        """Возвращает отсутствующие стадии, не считая EOF обязательным для streaming."""

        missing: list[str] = []
        if self.headers_ms is None:
            missing.append("headers")
        if self.first_body_ms is None:
            missing.append("first_body")
        if self.body_complete_ms is None and self.cancelled_ms is None:
            missing.append("body_complete")
        if self.ambiguous:
            missing.append("request_correlation")
        return missing

    def to_dict(self) -> dict[str, object]:
        """Строит secret-safe JSON row."""

        return {
            "sequence": self.sequence,
            "source": self.source,
            "request_id": self.safe_request_id,
            "operation_kind": self.operation_kind,
            "owner_seek_sequence": self.owner_seek_sequence,
            "request_to_headers_ms": self.headers_ms,
            "request_to_first_byte_ms": self.first_body_ms,
            "request_to_body_complete_ms": self.body_complete_ms,
            "request_to_cancelled_ms": self.cancelled_ms,
            "request_to_terminal_ms": self.terminal_ms,
            "terminal_outcome": self.terminal_outcome,
            "terminal_error_category": self.terminal_error_category,
            "body_bytes": self.body_bytes,
            "ambiguous": self.ambiguous,
            "missing_stages": self.missing_stages(),
        }


@dataclass(frozen=True)
class NetworkTerminalAnomaly:
    """Secret-safe terminal marker, который нельзя честно привязать к request outcome."""

    sequence: int
    source: str
    kind: str
    safe_request_id: str | None
    outcome: str | None
    elapsed_ms: float | None
    received_bytes: int | None
    line_number: int
    owner_seek_sequence: int | None
    impact: str

    def proof_relevant(self) -> bool:
        """Возвращает strict impact без вывода из текста anomaly kind."""

        return self.impact == "proof_relevant"

    def to_dict(self) -> dict[str, object]:
        """Сериализует additive evidence без URL, headers и абсолютного времени."""

        return {
            "sequence": self.sequence,
            "source": self.source,
            "anomaly_kind": self.kind,
            "request_id": self.safe_request_id,
            "terminal_outcome": self.outcome,
            "request_to_terminal_ms": self.elapsed_ms,
            "received_bytes": self.received_bytes,
            "line_number": self.line_number,
            "owner_seek_sequence": self.owner_seek_sequence,
            "impact": self.impact,
            "proof_relevant": self.proof_relevant(),
        }


@dataclass
class SeekSample:
    """Одна public seek/scrub operation до presentation/audio/commit."""

    sequence: int
    source: str
    role: str
    public_point: LogPoint | None = None
    enqueue_point: LogPoint | None = None
    receipt_point: LogPoint | None = None
    decoded_point: LogPoint | None = None
    presented_point: LogPoint | None = None
    audio_point: LogPoint | None = None
    commit_point: LogPoint | None = None
    progress_point: LogPoint | None = None
    progress_position_ms: float | None = None
    progress_delta_us: float | None = None
    generation: str = ""
    origin_ms: float | None = None
    target_ms: float | None = None
    actual_ms: float | None = None
    frame_pts_ms: float | None = None
    selected_video: str = ""
    selected_audio: str = ""
    available_audio_track_count: int | None = None
    demux_accepted: bool = False
    stale_frame: bool = False
    pre_target_presented_count: int | None = None
    explicit_failures: list[str] = field(default_factory=list)
    blocker: str = ""
    blocker_age_ms: float = 0.0
    superseded: bool = False
    superseded_point: LogPoint | None = None
    superseded_after_ms: float | None = None
    superseded_after_wall_ms: float | None = None
    supersede_network_status: str = "not_applicable"
    repeated: bool = False
    direction: str = "unknown"
    worker_round_trip_ms: float | None = None
    receipt_to_presented_ms: float | None = None
    receipt_to_audio_ms: float | None = None
    public_to_enqueue_ms: float | None = None
    public_to_presented_direct_ms: float | None = None
    public_to_audio_direct_ms: float | None = None
    public_to_commit_ms: float | None = None
    receipt_to_commit_ms: float | None = None
    public_to_progress_ms: float | None = None
    receipt_to_progress_ms: float | None = None
    commit_to_progress_ms: float | None = None
    scrub: ScrubTimeline | None = None
    network_request_sequences: list[int] = field(default_factory=list)

    def audio_required(self) -> bool:
        """Audio-less media не требует play; доступный audio track требует selection/play."""

        if self.available_audio_track_count == 0:
            return False
        if self.available_audio_track_count is not None:
            return self.available_audio_track_count > 0
        return self.selected_audio not in {"", "None", "null", "nil"}

    def audio_selection_known(self) -> bool:
        """Topology count либо explicit selection доказывают состояние audio path-а."""

        return self.available_audio_track_count is not None or self.selected_audio != ""

    def selected_audio_track_present(self) -> bool:
        """Не приравнивает существующий, но не выбранный audio track к audio-less media."""

        return self.selected_audio not in {"", "None", "null", "nil"}

    def video_required(self) -> bool:
        """Не требует video только при явно отсутствующем selected track."""

        return self.selected_video not in {"None", "null", "nil"}

    def correct_target_frame(self) -> bool:
        """Первый видимый frame обязан быть current generation и target/post-target."""

        if not self.video_required():
            return True
        if self.presented_point is None or self.stale_frame:
            return False
        if self.target_ms is None or self.frame_pts_ms is None:
            return False
        return self.frame_pts_ms + 0.001 >= self.target_ms

    def no_pre_target_proven(self) -> bool:
        """Нулевой owner counter — единственное положительное доказательство."""

        return self.pre_target_presented_count == 0

    def monotonic_public_to_presented_ms(self) -> float | None:
        """Собирает public→frame только из owner-monotonic spans."""

        if self.public_to_presented_direct_ms is not None:
            return self.public_to_presented_direct_ms
        if (
            self.public_to_enqueue_ms is None
            or self.worker_round_trip_ms is None
            or self.receipt_to_presented_ms is None
        ):
            return None
        return (
            self.public_to_enqueue_ms
            + self.worker_round_trip_ms
            + self.receipt_to_presented_ms
        )

    def monotonic_public_to_audio_ms(self) -> float | None:
        """Собирает public→audio только из owner-monotonic spans."""

        if not self.audio_selection_known():
            return None
        if not self.audio_required():
            return 0.0
        if self.public_to_audio_direct_ms is not None:
            return self.public_to_audio_direct_ms
        if (
            self.public_to_enqueue_ms is None
            or self.worker_round_trip_ms is None
            or self.receipt_to_audio_ms is None
        ):
            return None
        return (
            self.public_to_enqueue_ms
            + self.worker_round_trip_ms
            + self.receipt_to_audio_ms
        )

    def monotonic_enqueue_to_presented_ms(self) -> float | None:
        """Сохраняет доступный в legacy logs worker enqueue→frame subspan."""

        if self.worker_round_trip_ms is None or self.receipt_to_presented_ms is None:
            return None
        return self.worker_round_trip_ms + self.receipt_to_presented_ms

    def monotonic_enqueue_to_audio_ms(self) -> float | None:
        """Сохраняет доступный в legacy logs worker enqueue→audio subspan."""

        if not self.audio_selection_known():
            return None
        if not self.audio_required():
            return 0.0
        if self.worker_round_trip_ms is None or self.receipt_to_audio_ms is None:
            return None
        return self.worker_round_trip_ms + self.receipt_to_audio_ms

    def monotonic_enqueue_to_ready_ms(self) -> float | None:
        """Показывает измеренный subpath, не называя worker enqueue public API."""

        presented_ms = self.monotonic_enqueue_to_presented_ms()
        audio_ms = self.monotonic_enqueue_to_audio_ms()
        if presented_ms is None or audio_ms is None:
            return None
        return max(presented_ms, audio_ms)

    def monotonic_public_to_ready_ms(self) -> float | None:
        """User-visible readiness наступает после video и selected audio."""

        presented_ms = self.monotonic_public_to_presented_ms()
        audio_ms = self.monotonic_public_to_audio_ms()
        if presented_ms is None or audio_ms is None:
            return None
        return max(presented_ms, audio_ms)

    def wall_public_to_ready_ms(self) -> float | None:
        """Диагностический fallback явно остаётся wall-clock, не monotonic."""

        ready_point = later_point(self.presented_point, self.audio_point)
        return wall_elapsed(self.public_point, ready_point)

    def missing_gates(self) -> list[str]:
        """Перечисляет каждое недоказанное требование final acceptance."""

        missing: list[str] = []
        if self.public_point is None:
            missing.append("public_seek_or_end_scrub_accepted")
        if self.enqueue_point is None:
            missing.append("worker_request_enqueued")
        if self.receipt_point is None:
            missing.append("worker_receipt")
        if not self.demux_accepted:
            missing.append("demux_accepted")
        if not self.audio_selection_known():
            missing.append("selected_audio_state")
        if self.audio_required() and not self.selected_audio_track_present():
            missing.append("selected_audio_track")
        if self.video_required() and self.decoded_point is None:
            missing.append("target_frame_decoded")
        if self.video_required() and not self.correct_target_frame():
            missing.append("correct_target_frame_presented")
        if self.audio_required() and self.audio_point is None:
            missing.append("audio_resumed")
        if self.commit_point is None:
            missing.append("final_commit")
        if self.progress_point is None:
            missing.append("position_progressed")
        if not self.no_pre_target_proven():
            missing.append("no_pre_target_presentation_proof")
        if self.monotonic_public_to_ready_ms() is None:
            missing.append("public_to_ready_monotonic_span")
        if self.role == "timeline_final":
            if self.scrub is not None and self.scrub.correlation_failures:
                missing.append("scrub_command_correlation")
            if self.scrub is None or self.scrub.begin is None:
                missing.append("begin_scrub")
            if self.scrub is None or not self.scrub.previews:
                missing.append("preview_scrub")
            if self.scrub is None or self.scrub.end is None:
                missing.append("end_scrub")
            if self.scrub is None or self.scrub.begin_to_end_ms is None:
                missing.append("begin_to_end_monotonic_span")
        return missing

    def order_failures(self) -> list[str]:
        """Фиксирует ложный UI commit до target frame/audio readiness."""

        failures: list[str] = []
        if self.commit_point is None:
            return failures
        if point_is_before(self.commit_point, self.presented_point):
            failures.append("commit_before_target_frame")
        if self.audio_required() and point_is_before(self.commit_point, self.audio_point):
            failures.append("commit_before_audio")
        if self.progress_point is not None and point_is_before(
            self.progress_point, self.commit_point
        ):
            failures.append("position_progress_before_commit")
        if self.progress_delta_us is not None and self.progress_delta_us <= 0.0:
            failures.append("position_did_not_advance")
        elif (
            self.progress_delta_us is None
            and self.progress_position_ms is not None
            and self.target_ms is not None
            and self.progress_position_ms <= self.target_ms
        ):
            failures.append("position_did_not_advance")
        if (
            self.target_ms is not None
            and self.target_ms > 0.0
            and self.actual_ms is not None
            and self.actual_ms <= 0.0
        ):
            failures.append("demux_landed_at_zero_for_nonzero_target")
        return failures

    def verdict(self) -> Verdict:
        """Явная ошибка сильнее missing telemetry; supersede остаётся отдельным исходом."""

        if self.superseded:
            return Verdict.SUPERSEDED
        if self.explicit_failures or self.order_failures():
            return Verdict.FAIL
        if self.missing_gates():
            return Verdict.INCOMPLETE
        return Verdict.PASS

    def to_dict(self) -> dict[str, object]:
        """Преобразует sample в стабильный report row."""

        return {
            "sequence": self.sequence,
            "source": self.source,
            "role": self.role,
            "generation": self.generation,
            "origin_ms": self.origin_ms,
            "target_ms": self.target_ms,
            "actual_ms": self.actual_ms,
            "frame_pts_ms": self.frame_pts_ms,
            "selected_audio": self.selected_audio,
            "available_audio_track_count": self.available_audio_track_count,
            "direction": self.direction,
            "repeated": self.repeated,
            "superseded": self.superseded,
            "superseded_after_ms": self.superseded_after_ms,
            "superseded_after_wall_ms": self.superseded_after_wall_ms,
            "supersede_network_status": self.supersede_network_status,
            "public_to_ready_ms": self.monotonic_public_to_ready_ms(),
            "public_to_ready_wall_ms": self.wall_public_to_ready_ms(),
            "enqueue_to_ready_ms": self.monotonic_enqueue_to_ready_ms(),
            "enqueue_to_receipt_ms": self.worker_round_trip_ms,
            "receipt_to_presented_ms": self.receipt_to_presented_ms,
            "receipt_to_audio_ms": self.receipt_to_audio_ms,
            "public_to_commit_ms": self.public_to_commit_ms,
            "receipt_to_commit_ms": self.receipt_to_commit_ms,
            "public_to_progress_ms": self.public_to_progress_ms,
            "receipt_to_progress_ms": self.receipt_to_progress_ms,
            "commit_to_progress_ms": self.commit_to_progress_ms,
            "av_ready_skew_ms": self.av_ready_skew_ms(),
            "progress_position_ms": self.progress_position_ms,
            "network_request_sequences": self.network_request_sequences,
            "blocker": self.blocker,
            "blocker_age_ms": self.blocker_age_ms,
            "missing_gates": self.missing_gates(),
            "failures": [*self.explicit_failures, *self.order_failures()],
            "verdict": self.verdict().value,
            "scrub": scrub_to_dict(self.scrub),
        }

    def av_ready_skew_ms(self) -> float | None:
        """Показывает разницу target-frame/audio readiness внутри receipt clock domain."""

        if self.receipt_to_presented_ms is None or self.receipt_to_audio_ms is None:
            return None
        return abs(self.receipt_to_presented_ms - self.receipt_to_audio_ms)


@dataclass
class ProcessRun:
    """Startup readiness одного process log."""

    source: str
    process_start: LogPoint | None = None
    media_open_accepted: LogPoint | None = None
    explicit_media_open_accepted: bool = False
    first_presented: LogPoint | None = None
    first_audio: LogPoint | None = None
    audio_output_ready: LogPoint | None = None
    audio_playback_resumed: LogPoint | None = None
    process_to_presented_direct_ms: float | None = None
    process_to_audio_direct_ms: float | None = None
    process_to_audio_output_direct_ms: float | None = None
    structured_startup_attempt_id: int | None = None
    structured_startup_target: str | None = None
    structured_playback_expectation: StartupPlaybackExpectation | None = None
    structured_audio_expectation: StartupAudioExpectation | None = None
    structured_final_point: LogPoint | None = None
    structured_process_to_ready_ms: float | None = None
    structured_media_to_ready_ms: float | None = None
    explicit_failures: list[str] = field(default_factory=list)

    def begin_structured_startup(
        self,
        attempt_id: int,
        target: str,
        playback: StartupPlaybackExpectation,
        audio: StartupAudioExpectation,
    ) -> None:
        """Начинает exact attempt и очищает gates superseded startup-а."""

        self.structured_startup_attempt_id = attempt_id
        self.structured_startup_target = target
        self.structured_playback_expectation = playback
        self.structured_audio_expectation = audio
        self.structured_final_point = None
        self.structured_process_to_ready_ms = None
        self.structured_media_to_ready_ms = None
        self.first_presented = None
        self.first_audio = None
        self.audio_output_ready = None
        self.audio_playback_resumed = None
        self.process_to_presented_direct_ms = None
        self.process_to_audio_direct_ms = None
        self.process_to_audio_output_direct_ms = None

    def uses_structured_startup(self) -> bool:
        """Возвращает `True`, когда accepted marker включил strict correlation."""

        return self.structured_startup_attempt_id is not None

    def process_to_ready_ms(self) -> float | None:
        """Требует owner-monotonic process elapsed для обоих A/V gates."""

        if self.uses_structured_startup():
            return self.structured_process_to_ready_ms

        presented_ms = self.process_to_presented_direct_ms
        audio_ms = self.process_to_audio_direct_ms
        if presented_ms is None or audio_ms is None:
            return None
        return max(presented_ms, audio_ms)

    def media_open_to_ready_ms(self) -> float | None:
        """Вычитает process-relative monotonic points, если они оба опубликованы."""

        if self.uses_structured_startup():
            return self.structured_media_to_ready_ms

        ready_ms = self.process_to_ready_ms()
        accepted_ms = (
            self.media_open_accepted.process_elapsed_ms
            if self.media_open_accepted is not None
            else None
        )
        if ready_ms is None or accepted_ms is None:
            return None
        return ready_ms - accepted_ms

    def wall_process_to_ready_ms(self) -> float | None:
        """Показывает legacy wall-clock breakdown с явным названием clock basis."""

        return wall_elapsed(
            self.process_start, later_point(self.first_presented, self.first_audio)
        )

    def missing_gates(self) -> list[str]:
        """Не допускает успешный startup без обоих требуемых latency origins."""

        missing: list[str] = []
        if self.process_start is None:
            missing.append("process_start")
        if self.media_open_accepted is None:
            missing.append("media_open_or_restore_accepted")
        if self.first_presented is None:
            missing.append("first_presented_frame")
        if self.uses_structured_startup():
            if self.structured_audio_expectation == StartupAudioExpectation.NOT_PRESENT:
                if self.first_audio is None:
                    missing.append("audio_absence_proven")
            elif self.structured_audio_expectation == StartupAudioExpectation.UNKNOWN:
                missing.append("audio_expectation_resolved")
            elif self.structured_audio_expectation == StartupAudioExpectation.REQUIRED:
                if self.structured_playback_expectation == StartupPlaybackExpectation.PLAYING:
                    if self.audio_playback_resumed is None:
                        missing.append("audio_playback_resumed")
                elif self.structured_playback_expectation == StartupPlaybackExpectation.PAUSED:
                    if self.audio_output_ready is None:
                        missing.append("audio_output_ready")
                else:
                    missing.append("startup_playback_expectation")
            if self.structured_final_point is None:
                missing.append("startup_final_readiness_marker")
        elif self.first_audio is None:
            missing.append("audio_resumed")
        if self.process_to_ready_ms() is None:
            missing.append("process_to_ready_monotonic_span")
        if self.media_open_to_ready_ms() is None:
            missing.append("media_open_to_ready_monotonic_span")
        return missing

    def verdict(self) -> Verdict:
        """Startup explicit failure не скрывается за отсутствующими markers."""

        if self.explicit_failures:
            return Verdict.FAIL
        if self.missing_gates():
            return Verdict.INCOMPLETE
        return Verdict.PASS

    def to_dict(self) -> dict[str, object]:
        """Возвращает process-level latency row."""

        return {
            "source": self.source,
            "process_to_ready_ms": self.process_to_ready_ms(),
            "media_open_to_ready_ms": self.media_open_to_ready_ms(),
            "process_to_ready_wall_ms": self.wall_process_to_ready_ms(),
            "startup_attempt_id": self.structured_startup_attempt_id,
            "startup_target": self.structured_startup_target,
            "startup_playback_expectation": (
                self.structured_playback_expectation.value
                if self.structured_playback_expectation is not None
                else None
            ),
            "startup_audio_expectation": (
                self.structured_audio_expectation.value
                if self.structured_audio_expectation is not None
                else None
            ),
            "missing_gates": self.missing_gates(),
            "failures": self.explicit_failures,
            "verdict": self.verdict().value,
        }


def parse_timestamp_ms(line: str) -> float | None:
    """Парсит tracing ISO-8601 timestamp; строки без timestamp остаются валидными."""

    match = TIMESTAMP_PATTERN.match(line)
    if match is None:
        return None
    timestamp = match.group("timestamp").replace("Z", "+00:00")
    try:
        return datetime.fromisoformat(timestamp).timestamp() * 1000.0
    except ValueError:
        return None


def field_value(line: str, key: str) -> str | None:
    """Извлекает tracing-style key=value без интерпретации Rust debug type."""

    pattern = rf"\b{re.escape(key)}\s*=\s*(\"[^\"]*\"|'[^']*'|[^\s,]+)"
    match = re.search(pattern, line)
    if match is None:
        return None
    raw_value = match.group(1).strip().strip(",")
    if len(raw_value) >= 2 and raw_value[0] == raw_value[-1] and raw_value[0] in {"'", '"'}:
        return raw_value[1:-1]
    return raw_value


def startup_target_field(line: str) -> str | None:
    """Нормализует exact Beginning/Restore target из Rust Debug tracing field."""

    direct_value = field_value(line, "startup_target")
    if direct_value == "Beginning":
        return direct_value
    if direct_value is not None and direct_value.startswith("Restore {"):
        return " ".join(direct_value.split())
    restore_match = re.search(
        r"\bstartup_target\s*=\s*(Restore\s*\{\s*target_position:\s*[^}]+\})",
        line,
    )
    if restore_match is None:
        return None
    return " ".join(restore_match.group(1).split())


def startup_playback_expectation_field(
    line: str,
) -> StartupPlaybackExpectation | None:
    """Парсит только два поддержанных playback intent-а без эвристик."""

    value = field_value(line, "playback_expectation")
    try:
        return StartupPlaybackExpectation(value) if value is not None else None
    except ValueError:
        return None


def startup_audio_expectation_field(line: str) -> StartupAudioExpectation | None:
    """Парсит explicit audio expectation без вывода отсутствия из snapshot-а."""

    value = field_value(line, "audio_expectation")
    try:
        return StartupAudioExpectation(value) if value is not None else None
    except ValueError:
        return None


def float_field(line: str, *keys: str) -> float | None:
    """Возвращает первое корректное числовое field из списка aliases."""

    for key in keys:
        value = field_value(line, key)
        if value is None:
            continue
        try:
            return float(value)
        except ValueError:
            continue
    return None


def int_field(line: str, *keys: str) -> int | None:
    """Возвращает integer field, включая tracing values с десятичной точкой."""

    value = float_field(line, *keys)
    return None if value is None else int(value)


def bool_field(line: str, key: str) -> bool | None:
    """Парсит только явные true/false и не делает truthy guesses."""

    value = field_value(line, key)
    if value == "true":
        return True
    if value == "false":
        return False
    return None


def generation_from_line(line: str) -> str:
    """Нормализует seek generation aliases."""

    for key in ("generation", "active_seek_generation", "pipeline_generation"):
        value = field_value(line, key)
        if value is not None:
            return value
    return ""


def point_for_line(source: str, line_number: int, line: str) -> LogPoint:
    """Создаёт point и читает только process-global monotonic aliases."""

    return LogPoint(
        source=source,
        line_number=line_number,
        wall_timestamp_ms=parse_timestamp_ms(line),
        process_elapsed_ms=float_field(
            line, "process_elapsed_ms", "process_elapsed_milliseconds"
        ),
    )


def later_point(left: LogPoint | None, right: LogPoint | None) -> LogPoint | None:
    """Возвращает более поздний point; absent audio/video не считается готовностью."""

    if left is None or right is None:
        return None
    if left.wall_timestamp_ms is not None and right.wall_timestamp_ms is not None:
        return left if left.wall_timestamp_ms >= right.wall_timestamp_ms else right
    return left if left.line_number >= right.line_number else right


def point_is_before(left: LogPoint, right: LogPoint | None) -> bool:
    """Absent readiness считается ordering failure для уже опубликованного commit-а."""

    if right is None:
        return True
    if left.wall_timestamp_ms is not None and right.wall_timestamp_ms is not None:
        return left.wall_timestamp_ms < right.wall_timestamp_ms
    return left.line_number < right.line_number


def wall_elapsed(start: LogPoint | None, end: LogPoint | None) -> float | None:
    """Вычисляет legacy wall interval только при двух timestamped endpoints."""

    if (
        start is None
        or end is None
        or start.wall_timestamp_ms is None
        or end.wall_timestamp_ms is None
    ):
        return None
    return max(0.0, end.wall_timestamp_ms - start.wall_timestamp_ms)


def scrub_to_dict(scrub: ScrubTimeline | None) -> dict[str, object] | None:
    """Печатает timeline drag stages без абсолютных timestamps."""

    if scrub is None:
        return None
    return {
        "preview_count": len(scrub.previews),
        "begin_to_first_preview_ms": scrub.begin_to_first_preview_ms,
        "begin_to_end_ms": scrub.begin_to_end_ms,
        "correlation_failures": list(scrub.correlation_failures),
    }


def read_log_lines(path: Path) -> list[str]:
    """Читает log с replacement invalid UTF-8 без каких-либо side effects."""

    return path.read_text(encoding="utf-8", errors="replace").splitlines()
