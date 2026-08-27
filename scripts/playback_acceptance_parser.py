"""Stateful correlation для :mod:`playback_acceptance` telemetry model."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from pathlib import Path
from typing import Iterable

from playback_acceptance import (
    AUDIO_ACCEPTED_MARKER,
    COMMIT_MARKER,
    COMMIT_TIMEOUT_MARKER,
    DECODED_MARKER,
    DEMUX_ACCEPTED_MARKER,
    HTTP_BOUNDED_TERMINAL_MARKER,
    HTTP_BODY_COMPLETE_MARKER,
    HTTP_CANCELLED_MARKERS,
    HTTP_FIRST_BODY_MARKER,
    HTTP_HEADERS_MARKER,
    HTTP_REQUEST_MARKER,
    MEDIA_OPEN_ACCEPTED_MARKERS,
    POSITION_PROGRESS_MARKERS,
    PRE_TARGET_PRESENTED_MARKERS,
    PRESENTED_MARKER,
    PROCESS_START_MARKERS,
    ROLLBACK_MARKERS,
    SEEK_FAILURE_MARKERS,
    STARTUP_AUDIO_OUTPUT_READY_MARKER,
    STARTUP_AUDIO_NOT_REQUIRED_MARKER,
    STARTUP_AUDIO_RESUMED_MARKER,
    STARTUP_MEDIA_OPEN_ACCEPTED_MARKER,
    STARTUP_PRESENTED_MARKER,
    STARTUP_READY_MARKER,
    WAITING_MARKER,
    WORKER_RECEIPT_MARKER,
    WORKER_REQUEST_MARKER,
    LogPoint,
    NetworkTerminalAnomaly,
    ProcessRun,
    ScrubTimeline,
    SeekSample,
    StartupAudioExpectation,
    StartupPlaybackExpectation,
    Verdict,
    bool_field,
    field_value,
    float_field,
    generation_from_line,
    int_field,
    point_for_line,
    read_log_lines,
    startup_audio_expectation_field,
    startup_playback_expectation_field,
    startup_target_field,
    wall_elapsed,
)

from playback_acceptance_network import NetworkTracker
from playback_acceptance_hls import (
    HLS_MANIFEST_SEGMENT_SEEK_MARKER,
    HlsManifestSelectionAnomaly,
    HlsManifestSelectionRecord,
    HlsManifestSelectionTracker,
    hls_manifest_selection_anomaly_summary,
    hls_manifest_selection_summary_rows,
)
from playback_acceptance_scrub import (
    ScrubCommandAnomaly,
    ScrubCommandCorrelationTracker,
    ScrubCommandMarker,
    ScrubCommandStage,
    ScrubCorrelationAction,
    line_is_scrub_command_marker,
    parse_scrub_command_marker,
)
from playback_acceptance_summary import MetricSummary, summary_for_values
from seek_diagnostics_metrics import nearest_rank


# PTY capture сохраняет SGR/CSI styling tracing-subscriber-а прямо между
# field-name, `=` и value. Эти control bytes не являются частью telemetry.
ANSI_CSI_PATTERN = re.compile(r"\x1b\[[0-?]*[ -/]*[@-~]")
PUBLIC_COMMAND_MARKER = "Player command received command="
PUBLIC_FINAL_SEEK_ACCEPTED_MARKER = "Public final seek accepted"
PUBLIC_COMMAND_TARGET_PATTERN = re.compile(
    r"target: Absolute\(MediaTime\((?P<value>[0-9.]+)(?P<unit>ms|s)\)"
)
RELEVANT_MARKERS = (
    *PROCESS_START_MARKERS,
    *MEDIA_OPEN_ACCEPTED_MARKERS,
    STARTUP_PRESENTED_MARKER,
    STARTUP_AUDIO_OUTPUT_READY_MARKER,
    STARTUP_AUDIO_RESUMED_MARKER,
    STARTUP_AUDIO_NOT_REQUIRED_MARKER,
    STARTUP_READY_MARKER,
    WORKER_REQUEST_MARKER,
    WORKER_RECEIPT_MARKER,
    DEMUX_ACCEPTED_MARKER,
    PRESENTED_MARKER,
    DECODED_MARKER,
    AUDIO_ACCEPTED_MARKER,
    COMMIT_MARKER,
    COMMIT_TIMEOUT_MARKER,
    WAITING_MARKER,
    HTTP_REQUEST_MARKER,
    HTTP_HEADERS_MARKER,
    HTTP_FIRST_BODY_MARKER,
    HTTP_BODY_COMPLETE_MARKER,
    HTTP_BOUNDED_TERMINAL_MARKER,
    *HTTP_CANCELLED_MARKERS,
    *POSITION_PROGRESS_MARKERS,
    *PRE_TARGET_PRESENTED_MARKERS,
    *SEEK_FAILURE_MARKERS,
    *ROLLBACK_MARKERS,
    PUBLIC_FINAL_SEEK_ACCEPTED_MARKER,
)
PRODUCTION_MARKER_REQUIREMENTS = (
    "process start origin plus process_elapsed_ms on first surface-presented "
    "startup frame and the typed Playing/Paused audio gate",
    "media-open/restore accepted with process_elapsed_ms, startup_attempt_id, "
    "exact target and playback expectation",
    "final startup readiness with the same attempt/target/playback expectation "
    "and owner-monotonic process/media spans",
    "public Seek/EndScrub accepted plus public_to_enqueue_ms or direct public_to_presented/audio spans",
    "presented_pre_target_frames=0 for the committed generation",
    "post-commit PositionChanged/progress marker with generation and position_ms",
    "HTTP request_id/http_request_id/resource_request_id on start, headers, first body, EOF and cancellation",
    "secret-safe resource/segment identity, purpose and cache-hit marker for repeated target reuse",
    "paired INFO dispatch/acceptance scrub_schema_version=1 markers with exact monotonic "
    "scrub_command_id, stage and requested target identity",
    "committed kind=hls_manifest_segment_seek marker with exact HLS-local selection ID, "
    "phase/component role, requested target, selected half-open manifest interval and "
    "packet-derived actual/decode anchors; this evidence is never joined to a public operation",
)


def append_unique_failure(run: ProcessRun, message: str) -> None:
    """Добавляет bounded diagnostic один раз для одного process run."""

    if message not in run.explicit_failures:
        run.explicit_failures.append(message)


@dataclass
class PublicCommandObservation:
    """Logical command, уже применённая к parser state ровно один раз."""

    stage: ScrubCommandStage
    scrub: ScrubTimeline
    sample: SeekSample | None = None


@dataclass
class SourceState:
    """Correlation state строго одного process log."""

    run: ProcessRun
    network_tracker: NetworkTracker
    hls_manifest_selection_tracker: HlsManifestSelectionTracker
    scrub_tracker: ScrubCommandCorrelationTracker
    samples: list[SeekSample] = field(default_factory=list)
    active_by_generation: dict[str, SeekSample] = field(default_factory=dict)
    pending_public_sample: SeekSample | None = None
    current_sample: SeekSample | None = None
    scrub: ScrubTimeline | None = None
    scrub_observations: dict[str, PublicCommandObservation] = field(default_factory=dict)
    last_requested_target_ms: float | None = None
    last_committed_target_ms: float | None = None


class PlaybackAcceptanceAnalyzer:
    """Собирает startup/seek/HTTP и независимое HLS evidence из готовых логов."""

    def __init__(self, scenario: str = "") -> None:
        self.scenario = scenario
        self.runs: list[ProcessRun] = []
        self.samples: list[SeekSample] = []
        self.network_requests: list[NetworkRequest] = []
        self.network_terminal_anomalies: list[NetworkTerminalAnomaly] = []
        self.scrub_command_anomalies: list[ScrubCommandAnomaly] = []
        self.hls_manifest_selections: list[HlsManifestSelectionRecord] = []
        self.hls_manifest_selection_anomalies: list[
            HlsManifestSelectionAnomaly
        ] = []

    def parse_path(self, path: Path) -> None:
        """Читает один log; приложение и пользовательский профиль не затрагиваются."""

        self.parse_lines(read_log_lines(path), str(path))

    def parse_lines(self, lines: Iterable[str], source: str) -> None:
        """Парсит один source как отдельный process run."""

        state = SourceState(
            run=ProcessRun(source=source),
            network_tracker=NetworkTracker(
                source=source,
                first_sequence=len(self.network_requests) + 1,
                first_anomaly_sequence=len(self.network_terminal_anomalies) + 1,
            ),
            hls_manifest_selection_tracker=HlsManifestSelectionTracker(
                source=source,
                first_record_sequence=len(self.hls_manifest_selections) + 1,
                first_anomaly_sequence=len(self.hls_manifest_selection_anomalies)
                + 1,
            ),
            scrub_tracker=ScrubCommandCorrelationTracker(
                source=source,
                first_anomaly_sequence=len(self.scrub_command_anomalies) + 1,
            ),
        )
        for line_number, line in enumerate(lines, start=1):
            normalized_line = ANSI_CSI_PATTERN.sub("", line.rstrip("\n"))
            self._parse_line(state, source, line_number, normalized_line)
        self._finish_source(state)

    def _parse_line(
        self,
        state: SourceState,
        source: str,
        line_number: int,
        line: str,
    ) -> None:
        """Маршрутизирует строку к process/public/network/seek owners."""

        if not line_is_relevant(line):
            return
        if state.hls_manifest_selection_tracker.observe(line_number, line):
            return
        point = point_for_line(source, line_number, line)
        self._observe_process_marker(state, point, line)
        if self._observe_public_command(state, point, line):
            return
        if self._observe_network_marker(state, point, line):
            return
        if WORKER_REQUEST_MARKER in line:
            self._observe_worker_request(state, point, line)
            return

        sample = self._sample_for_line(state, line)
        if sample is None:
            return
        self._observe_sample_marker(state, sample, point, line)

    def _observe_process_marker(
        self, state: SourceState, point: LogPoint, line: str
    ) -> None:
        """Собирает process/media-open/A/V endpoints независимо от seek correlation."""

        if state.run.process_start is None and any(
            marker in line for marker in PROCESS_START_MARKERS
        ):
            state.run.process_start = point
        if STARTUP_MEDIA_OPEN_ACCEPTED_MARKER in line:
            state.run.media_open_accepted = point
            state.run.explicit_media_open_accepted = True
            self._observe_structured_startup_acceptance(state.run, line)
        elif (
            state.run.media_open_accepted is None
            and not state.run.explicit_media_open_accepted
            and any(marker in line for marker in MEDIA_OPEN_ACCEPTED_MARKERS)
        ):
            state.run.media_open_accepted = point
        if STARTUP_PRESENTED_MARKER in line:
            self._observe_startup_presented(state.run, point, line)
        elif (
            not state.run.uses_structured_startup()
            and PRESENTED_MARKER in line
            and state.run.first_presented is None
        ):
            state.run.first_presented = point
            state.run.process_to_presented_direct_ms = float_field(
                line,
                "process_to_presented_ms",
                "process_elapsed_ms",
                "process_elapsed_milliseconds",
            )
        if STARTUP_AUDIO_OUTPUT_READY_MARKER in line:
            self._observe_startup_audio_output_ready(state.run, point, line)
        elif STARTUP_AUDIO_RESUMED_MARKER in line:
            self._observe_startup_audio_resumed(state.run, point, line)
        elif STARTUP_AUDIO_NOT_REQUIRED_MARKER in line:
            self._observe_startup_audio_not_required(state.run, point, line)
        elif (
            not state.run.uses_structured_startup()
            and AUDIO_ACCEPTED_MARKER in line
            and state.run.first_audio is None
        ):
            state.run.first_audio = point
            state.run.process_to_audio_direct_ms = float_field(
                line,
                "process_to_audio_ms",
                "process_elapsed_ms",
                "process_elapsed_milliseconds",
            )
        if STARTUP_READY_MARKER in line:
            self._observe_structured_startup_ready(state.run, point, line)

    @staticmethod
    def _observe_structured_startup_acceptance(run: ProcessRun, line: str) -> None:
        """Включает strict mode только для полностью structured accepted marker-а."""

        attempt_id = int_field(line, "startup_attempt_id")
        target = startup_target_field(line)
        playback = startup_playback_expectation_field(line)
        audio = startup_audio_expectation_field(line)
        structured_values = (attempt_id, target, playback, audio)
        if all(value is None for value in structured_values):
            return
        if any(value is None for value in structured_values):
            append_unique_failure(
                run,
                "structured startup accepted marker is missing attempt/target/playback/audio",
            )
            return
        assert attempt_id is not None
        assert target is not None
        assert playback is not None
        assert audio is not None
        run.begin_structured_startup(attempt_id, target, playback, audio)

    @staticmethod
    def _structured_startup_marker_matches(run: ProcessRun, line: str) -> bool:
        """Проверяет exact attempt id; stale/missing marker никогда не закрывает gate."""

        if not run.uses_structured_startup():
            return False
        marker_attempt_id = int_field(line, "startup_attempt_id")
        if marker_attempt_id == run.structured_startup_attempt_id:
            return True
        append_unique_failure(run, "startup marker attempt id mismatch")
        return False

    def _observe_startup_presented(
        self, run: ProcessRun, point: LogPoint, line: str
    ) -> None:
        """Принимает surface endpoint только от active structured attempt-а."""

        if run.uses_structured_startup() and not self._structured_startup_marker_matches(
            run, line
        ):
            return
        run.first_presented = point
        run.process_to_presented_direct_ms = float_field(
            line,
            "process_to_presented_ms",
            "process_elapsed_ms",
            "process_elapsed_milliseconds",
        )

    def _observe_startup_audio_output_ready(
        self, run: ProcessRun, point: LogPoint, line: str
    ) -> None:
        """Сохраняет output readiness, но открывает audio gate только для Paused."""

        if not self._structured_startup_marker_matches(run, line):
            return
        marker_playback = startup_playback_expectation_field(line)
        if marker_playback != run.structured_playback_expectation:
            append_unique_failure(run, "startup audio-output playback expectation mismatch")
            return
        if run.structured_audio_expectation == StartupAudioExpectation.UNKNOWN:
            run.structured_audio_expectation = StartupAudioExpectation.REQUIRED
        if run.structured_audio_expectation != StartupAudioExpectation.REQUIRED:
            append_unique_failure(run, "startup audio output contradicts audio expectation")
            return
        run.audio_output_ready = point
        run.process_to_audio_output_direct_ms = float_field(
            line,
            "process_to_audio_output_ms",
            "process_elapsed_ms",
            "process_elapsed_milliseconds",
        )
        if run.structured_playback_expectation == StartupPlaybackExpectation.PAUSED:
            run.first_audio = point
            run.process_to_audio_direct_ms = run.process_to_audio_output_direct_ms

    def _observe_startup_audio_resumed(
        self, run: ProcessRun, point: LogPoint, line: str
    ) -> None:
        """Playing требует successful resume; Paused resume считается противоречием."""

        if run.uses_structured_startup():
            if not self._structured_startup_marker_matches(run, line):
                return
            if run.structured_playback_expectation != StartupPlaybackExpectation.PLAYING:
                append_unique_failure(run, "paused startup unexpectedly resumed audio")
                return
            if run.structured_audio_expectation == StartupAudioExpectation.UNKNOWN:
                run.structured_audio_expectation = StartupAudioExpectation.REQUIRED
            if run.structured_audio_expectation != StartupAudioExpectation.REQUIRED:
                append_unique_failure(run, "startup audio resume contradicts audio expectation")
                return
            run.audio_playback_resumed = point
        run.first_audio = point
        run.process_to_audio_direct_ms = float_field(
            line,
            "process_to_audio_ms",
            "process_elapsed_ms",
            "process_elapsed_milliseconds",
        )

    def _observe_startup_audio_not_required(
        self, run: ProcessRun, point: LogPoint, line: str
    ) -> None:
        """Audio-less gate принимается только при accepted `NotPresent`."""

        if run.uses_structured_startup():
            if not self._structured_startup_marker_matches(run, line):
                return
            if run.structured_audio_expectation == StartupAudioExpectation.UNKNOWN:
                run.structured_audio_expectation = StartupAudioExpectation.NOT_PRESENT
            if run.structured_audio_expectation != StartupAudioExpectation.NOT_PRESENT:
                append_unique_failure(run, "startup audio-less marker contradicts expectation")
                return
        run.first_audio = point
        run.process_to_audio_direct_ms = float_field(
            line,
            "process_to_audio_ms",
            "process_elapsed_ms",
            "process_elapsed_milliseconds",
        )

    def _observe_structured_startup_ready(
        self, run: ProcessRun, point: LogPoint, line: str
    ) -> None:
        """Принимает final marker только после exact correlation и фактических gates."""

        if not self._structured_startup_marker_matches(run, line):
            return
        final_target = startup_target_field(line)
        final_playback = startup_playback_expectation_field(line)
        final_audio = startup_audio_expectation_field(line)
        if final_target != run.structured_startup_target:
            append_unique_failure(run, "startup final target mismatch")
            return
        if final_playback != run.structured_playback_expectation:
            append_unique_failure(run, "startup final playback expectation mismatch")
            return
        if final_audio != run.structured_audio_expectation:
            append_unique_failure(run, "startup final audio expectation mismatch")
            return

        audio_gate_ready = (
            run.structured_audio_expectation == StartupAudioExpectation.NOT_PRESENT
            and run.first_audio is not None
        ) or (
            run.structured_audio_expectation == StartupAudioExpectation.REQUIRED
            and run.structured_playback_expectation == StartupPlaybackExpectation.PAUSED
            and run.audio_output_ready is not None
        ) or (
            run.structured_audio_expectation == StartupAudioExpectation.REQUIRED
            and run.structured_playback_expectation == StartupPlaybackExpectation.PLAYING
            and run.audio_playback_resumed is not None
        )
        process_to_ready_ms = float_field(line, "process_to_ready_ms")
        media_to_ready_ms = float_field(line, "media_to_ready_ms")
        audio_endpoint_ms = (
            run.process_to_audio_output_direct_ms
            if run.structured_audio_expectation == StartupAudioExpectation.REQUIRED
            and run.structured_playback_expectation == StartupPlaybackExpectation.PAUSED
            else run.process_to_audio_direct_ms
        )
        component_ready_ms = (
            max(run.process_to_presented_direct_ms, audio_endpoint_ms)
            if run.process_to_presented_direct_ms is not None
            and audio_endpoint_ms is not None
            else None
        )
        if (
            run.first_presented is None
            or not audio_gate_ready
            or process_to_ready_ms is None
            or media_to_ready_ms is None
            or component_ready_ms is None
            or process_to_ready_ms < component_ready_ms
        ):
            append_unique_failure(run, "startup final marker preceded required readiness gates")
            return
        run.structured_final_point = point
        run.structured_process_to_ready_ms = process_to_ready_ms
        run.structured_media_to_ready_ms = media_to_ready_ms

    def _observe_public_command(
        self, state: SourceState, point: LogPoint, line: str
    ) -> bool:
        """Начинает public operation на receipt command-а, а не на worker enqueue."""

        if PUBLIC_FINAL_SEEK_ACCEPTED_MARKER in line:
            current_sample = state.current_sample
            if (
                current_sample is not None
                and current_sample.role in {"seek", "timeline_final"}
                and current_sample.enqueue_point is None
                and current_sample.commit_point is None
            ):
                target_ms = float_field(line, "target_ms", "target_milliseconds")
                if target_ms is not None:
                    current_sample.target_ms = target_ms
                return True
            self._begin_public_sample(state, point, line, "seek", None)
            return True
        scrub_marker = (
            parse_scrub_command_marker(line, point)
            if line_is_scrub_command_marker(line)
            else None
        )
        if scrub_marker is None:
            if PUBLIC_COMMAND_MARKER not in line:
                return False
            if "command=Seek(" in line:
                self._begin_public_sample(state, point, line, "seek", None)
                return True
            return False

        decision = state.scrub_tracker.observe(scrub_marker)
        if decision.action == ScrubCorrelationAction.IGNORE_ANOMALOUS:
            return True
        if decision.action == ScrubCorrelationAction.ENRICH_EXISTING:
            if decision.correlation_key is not None:
                observation = state.scrub_observations.get(decision.correlation_key)
                if observation is not None:
                    self._enrich_paired_scrub_observation(observation, line)
            return True

        scrub_stage = scrub_marker.stage
        if scrub_stage is None:
            return True
        scrub_elapsed_ms = float_field(
            line,
            "scrub_elapsed_ms",
            "begin_to_preview_ms",
            "begin_to_end_ms",
            "elapsed_since_begin_ms",
        )

        if scrub_stage == ScrubCommandStage.BEGIN:
            state.scrub = ScrubTimeline(begin=point)
        elif scrub_stage in {ScrubCommandStage.PREVIEW, ScrubCommandStage.UPDATE}:
            if state.scrub is None:
                state.scrub = ScrubTimeline()
            state.scrub.previews.append(point)
            if state.scrub.begin_to_first_preview_ms is None:
                state.scrub.begin_to_first_preview_ms = scrub_elapsed_ms
        else:
            if state.scrub is None:
                state.scrub = ScrubTimeline()
            state.scrub.end = point
            state.scrub.begin_to_end_ms = scrub_elapsed_ms
            self._begin_public_sample(state, point, line, "timeline_final", state.scrub)
        if decision.correlation_key is not None:
            state.scrub_observations[decision.correlation_key] = PublicCommandObservation(
                stage=scrub_stage,
                scrub=state.scrub,
                sample=(
                    state.current_sample
                    if scrub_stage == ScrubCommandStage.END
                    else None
                ),
            )
        return True

    @staticmethod
    def _enrich_paired_scrub_observation(
        observation: PublicCommandObservation,
        line: str,
    ) -> None:
        """Добавляет typed spans к уже учтённой command без второго state transition."""

        if observation.stage in {
            ScrubCommandStage.PREVIEW,
            ScrubCommandStage.UPDATE,
        }:
            if observation.scrub.begin_to_first_preview_ms is None:
                observation.scrub.begin_to_first_preview_ms = float_field(
                    line,
                    "scrub_elapsed_ms",
                    "begin_to_preview_ms",
                    "elapsed_since_begin_ms",
                )
            return
        if observation.stage != ScrubCommandStage.END:
            return
        begin_to_end_ms = float_field(
            line,
            "scrub_elapsed_ms",
            "begin_to_end_ms",
            "elapsed_since_begin_ms",
        )
        if begin_to_end_ms is not None:
            observation.scrub.begin_to_end_ms = begin_to_end_ms
        if observation.sample is not None and observation.sample.origin_ms is None:
            observation.sample.origin_ms = float_field(line, "current_position_ms")

    def _begin_public_sample(
        self,
        state: SourceState,
        point: LogPoint,
        line: str,
        role: str,
        scrub: ScrubTimeline | None,
    ) -> None:
        """Создаёт sample и помечает предыдущую незавершённую операцию superseded."""

        if state.current_sample is not None and state.current_sample.commit_point is None:
            state.current_sample.superseded = True
            state.current_sample.superseded_point = point
            state.current_sample.superseded_after_ms = float_field(
                line, "supersede_after_ms"
            )
            state.current_sample.superseded_after_wall_ms = wall_elapsed(
                state.current_sample.public_point, point
            )
        sample = SeekSample(
            sequence=len(self.samples) + len(state.samples) + 1,
            source=state.run.source,
            role=role,
            generation=generation_from_line(line),
            public_point=point,
            origin_ms=float_field(line, "current_position_ms"),
            target_ms=target_from_public_command(line),
            scrub=scrub,
        )
        state.samples.append(sample)
        if sample.generation:
            state.active_by_generation[sample.generation] = sample
        state.pending_public_sample = sample
        state.current_sample = sample

    def _observe_worker_request(
        self, state: SourceState, point: LogPoint, line: str
    ) -> None:
        """Привязывает player/public operation к exact seek generation."""

        sample = state.pending_public_sample
        if sample is None:
            if state.current_sample is not None and state.current_sample.commit_point is None:
                state.current_sample.superseded = True
            sample = SeekSample(
                sequence=len(self.samples) + len(state.samples) + 1,
                source=state.run.source,
                role="worker_only",
            )
            state.samples.append(sample)
        state.pending_public_sample = None
        sample.enqueue_point = point
        sample.generation = generation_from_line(line) or f"line-{point.line_number}"
        request_target_ms = float_field(line, "target_milliseconds", "target_ms")
        if request_target_ms is not None:
            sample.target_ms = request_target_ms
        sample.public_to_enqueue_ms = float_field(
            line, "public_to_enqueue_ms", "accepted_to_enqueue_ms"
        )
        self._classify_direction(state, sample)
        state.current_sample = sample
        state.active_by_generation[sample.generation] = sample

    def _classify_direction(self, state: SourceState, sample: SeekSample) -> None:
        """Классифицирует forward/backward/repeated без догадки при неизвестном origin."""

        if sample.origin_ms is None:
            sample.origin_ms = state.last_committed_target_ms
        if sample.target_ms is not None and state.last_requested_target_ms is not None:
            sample.repeated = abs(sample.target_ms - state.last_requested_target_ms) < 0.001
        state.last_requested_target_ms = sample.target_ms
        if sample.target_ms is None or sample.origin_ms is None:
            sample.direction = "unknown"
        elif abs(sample.target_ms - sample.origin_ms) < 0.001:
            sample.direction = "same"
        elif sample.target_ms > sample.origin_ms:
            sample.direction = "forward"
        else:
            sample.direction = "backward"

    def _sample_for_line(self, state: SourceState, line: str) -> SeekSample | None:
        """Ищет exact generation, затем допускает только текущий active sample."""

        generation = generation_from_line(line)
        if generation and generation in state.active_by_generation:
            return state.active_by_generation[generation]
        if (
            state.current_sample is not None
            and state.current_sample.commit_point is None
            and not state.current_sample.superseded
        ):
            return state.current_sample
        return None

    def _observe_sample_marker(
        self,
        state: SourceState,
        sample: SeekSample,
        point: LogPoint,
        line: str,
    ) -> None:
        """Применяет marker к typed gates, timings и failures."""

        self._observe_common_sample_fields(sample, line)
        if WORKER_RECEIPT_MARKER in line:
            sample.receipt_point = point
            sample.worker_round_trip_ms = float_field(
                line, "elapsed_milliseconds", "enqueue_to_receipt_ms"
            )
        elif DEMUX_ACCEPTED_MARKER in line:
            sample.demux_accepted = True
        elif DECODED_MARKER in line:
            sample.decoded_point = point
        elif PRESENTED_MARKER in line:
            sample.presented_point = point
            sample.public_to_presented_direct_ms = float_field(
                line, "public_to_presented_ms"
            )
            sample.receipt_to_presented_ms = receipt_subpath_field(
                line,
                explicit_name="receipt_to_presented_ms",
                public_span=sample.public_to_presented_direct_ms,
                legacy_names=("elapsed_ms", "seek_elapsed_ms"),
            )
        elif AUDIO_ACCEPTED_MARKER in line:
            sample.audio_point = point
            sample.public_to_audio_direct_ms = float_field(line, "public_to_audio_ms")
            sample.receipt_to_audio_ms = receipt_subpath_field(
                line,
                explicit_name="receipt_to_audio_ms",
                public_span=sample.public_to_audio_direct_ms,
                legacy_names=("accepted_after_ms", "seek_elapsed_ms"),
            )
        elif COMMIT_MARKER in line:
            sample.commit_point = point
            sample.public_to_commit_ms = float_field(line, "public_to_commit_ms")
            sample.receipt_to_commit_ms = float_field(line, "receipt_to_commit_ms")
            state.last_committed_target_ms = sample.target_ms
        elif COMMIT_TIMEOUT_MARKER in line:
            sample.explicit_failures.append("commit_timeout")
        elif WAITING_MARKER in line:
            sample.blocker = field_value(line, "blocker") or sample.blocker
            sample.blocker_age_ms = max(
                sample.blocker_age_ms, float_field(line, "age_ms") or 0.0
            )
        elif any(marker in line for marker in POSITION_PROGRESS_MARKERS):
            sample.progress_point = point
            sample.progress_position_ms = float_field(
                line, "position_ms", "current_position_ms"
            )
            sample.progress_delta_us = float_field(line, "progress_delta_us")
            sample.public_to_progress_ms = float_field(line, "public_to_progress_ms")
            sample.receipt_to_progress_ms = float_field(line, "receipt_to_progress_ms")
            sample.commit_to_progress_ms = float_field(line, "commit_to_progress_ms")
        elif any(marker in line for marker in PRE_TARGET_PRESENTED_MARKERS):
            sample.explicit_failures.append("pre_target_frame_presented")
        elif any(marker in line for marker in SEEK_FAILURE_MARKERS):
            sample.explicit_failures.append("seek_unavailable_or_worker_failure")
        elif any(marker in line for marker in ROLLBACK_MARKERS):
            sample.explicit_failures.append("position_rollback")

    def _observe_common_sample_fields(self, sample: SeekSample, line: str) -> None:
        """Сохраняет fields, которые могут публиковаться на разных markers."""

        target_ms = float_field(line, "target_ms", "target_milliseconds")
        if target_ms is not None:
            sample.target_ms = target_ms
        actual_ms = float_field(line, "actual_ms", "actual_milliseconds")
        if actual_ms is not None:
            sample.actual_ms = actual_ms
        frame_pts_ms = float_field(line, "frame_pts_ms")
        if frame_pts_ms is not None:
            sample.frame_pts_ms = frame_pts_ms
        sample.selected_video = (
            field_value(line, "selected_video_track_id") or sample.selected_video
        )
        sample.selected_audio = (
            field_value(line, "selected_audio_track_id") or sample.selected_audio
        )
        available_audio_track_count = int_field(line, "available_audio_track_count")
        if available_audio_track_count is not None:
            sample.available_audio_track_count = available_audio_track_count
        stale_frame = bool_field(line, "stale_frame")
        if stale_frame is not None:
            sample.stale_frame = sample.stale_frame or stale_frame
        pre_target_count = int_field(
            line, "presented_pre_target_frames", "pre_target_frames_presented"
        )
        if pre_target_count is not None:
            sample.pre_target_presented_count = max(
                sample.pre_target_presented_count or 0, pre_target_count
            )
            if (
                pre_target_count > 0
                and "pre_target_frame_presented" not in sample.explicit_failures
            ):
                sample.explicit_failures.append("pre_target_frame_presented")

    def _observe_network_marker(
        self, state: SourceState, point: LogPoint, line: str
    ) -> bool:
        """Делегирует HTTP lifecycle отдельному transport diagnostics owner-у."""

        owner_sequence = (
            state.current_sample.sequence
            if state.current_sample is not None
            and state.current_sample.commit_point is None
            else None
        )
        observation = state.network_tracker.observe(point, line, owner_sequence)
        if (
            observation.started_request is not None
            and state.current_sample is not None
            and observation.started_request.owner_seek_sequence is not None
        ):
            state.current_sample.network_request_sequences.append(
                observation.started_request.sequence
            )
        return observation.handled

    def _finish_source(self, state: SourceState) -> None:
        """Публикует source rows и переносит terminal seek failure на startup run."""

        state.scrub_tracker.finish()
        self._apply_scrub_correlation_anomalies(state)
        first_final = next(
            (sample for sample in state.samples if sample.role != "preview_scrub"), None
        )
        if first_final is not None and first_final.verdict() == Verdict.FAIL:
            state.run.explicit_failures.extend(first_final.explicit_failures)
        for sample in state.samples:
            self._settle_supersede_network_status(state, sample)
        self.runs.append(state.run)
        self.samples.extend(state.samples)
        self.network_requests.extend(state.network_tracker.requests)
        self.network_terminal_anomalies.extend(
            state.network_tracker.terminal_anomalies
        )
        self.scrub_command_anomalies.extend(state.scrub_tracker.anomalies)
        self.hls_manifest_selections.extend(
            state.hls_manifest_selection_tracker.records
        )
        self.hls_manifest_selection_anomalies.extend(
            state.hls_manifest_selection_tracker.anomalies
        )

    @staticmethod
    def _apply_scrub_correlation_anomalies(state: SourceState) -> None:
        """Делает affected drag non-eligible, сохраняя отдельный anomaly evidence."""

        for anomaly in state.scrub_tracker.anomalies:
            if anomaly.correlation_key is not None:
                observation = state.scrub_observations.get(anomaly.correlation_key)
                scrub_timelines = [observation.scrub] if observation is not None else []
            else:
                scrub_timelines = [
                    observation.scrub
                    for observation in state.scrub_observations.values()
                ]
                if not scrub_timelines and state.scrub is not None:
                    scrub_timelines = [state.scrub]
            seen_timelines: set[int] = set()
            for scrub in scrub_timelines:
                identity = id(scrub)
                if identity in seen_timelines:
                    continue
                seen_timelines.add(identity)
                if anomaly.kind not in scrub.correlation_failures:
                    scrub.correlation_failures.append(anomaly.kind)

    def _settle_supersede_network_status(
        self, state: SourceState, sample: SeekSample
    ) -> None:
        """Доказывает cancel либо EOF obsolete requests после supersede."""

        if not sample.superseded:
            return
        owned_requests = [
            request
            for request in state.network_tracker.requests
            if request.sequence in sample.network_request_sequences
        ]
        if not owned_requests:
            sample.supersede_network_status = "no_owned_request_observed"
            return
        if sample.superseded_point is None:
            sample.supersede_network_status = "missing_supersede_point"
            return
        if all(
            request.cancelled_point is not None
            or point_not_after(request.body_complete_point, sample.superseded_point)
            for request in owned_requests
        ):
            sample.supersede_network_status = "cancelled_or_completed_before_supersede"
        else:
            sample.supersede_network_status = "cancellation_unproven"

    def summary_rows(self) -> list[MetricSummary]:
        """Возвращает requested seek summaries; superseded samples видны в counts."""

        final_samples = [sample for sample in self.samples if sample.role != "preview_scrub"]
        rows = [
            summary_for_values(
                "public_to_ready_ms",
                final_samples,
                SeekSample.monotonic_public_to_ready_ms,
                clock_basis="owner_monotonic",
            ),
            summary_for_values(
                "public_to_presented_ms",
                final_samples,
                SeekSample.monotonic_public_to_presented_ms,
                clock_basis="owner_monotonic",
            ),
            summary_for_values(
                "public_to_audio_ms",
                final_samples,
                SeekSample.monotonic_public_to_audio_ms,
                clock_basis="owner_monotonic",
            ),
            summary_for_values(
                "public_to_ready_wall_ms",
                final_samples,
                SeekSample.wall_public_to_ready_ms,
                clock_basis="wall_timestamp_diagnostic_only",
            ),
            summary_for_values(
                "enqueue_to_ready_ms",
                final_samples,
                SeekSample.monotonic_enqueue_to_ready_ms,
                clock_basis="owner_monotonic_subpath",
            ),
        ]
        timeline_samples = [
            sample for sample in final_samples if sample.role == "timeline_final"
        ]
        rows.extend(
            (
                summary_for_values(
                    "scrub_begin_to_first_preview_ms",
                    timeline_samples,
                    scrub_begin_to_first_preview_ms,
                    clock_basis="owner_monotonic",
                ),
                summary_for_values(
                    "scrub_begin_to_end_ms",
                    timeline_samples,
                    scrub_begin_to_end_ms,
                    clock_basis="owner_monotonic",
                ),
            )
        )
        return rows

    def startup_summary_rows(self) -> list[dict[str, object]]:
        """Считает startup p50/p95/max только по полностью доказанным PASS runs."""

        rows: list[dict[str, object]] = []
        for metric, getter, clock_basis in (
            ("process_to_ready_ms", ProcessRun.process_to_ready_ms, "owner_monotonic"),
            (
                "media_open_to_ready_ms",
                ProcessRun.media_open_to_ready_ms,
                "owner_monotonic",
            ),
            (
                "process_to_ready_wall_ms",
                ProcessRun.wall_process_to_ready_ms,
                "wall_timestamp_diagnostic_only",
            ),
        ):
            observed_values = [
                value
                for run in self.runs
                if (value := getter(run)) is not None
            ]
            eligible_values = [
                value
                for run in self.runs
                if run.verdict() == Verdict.PASS and (value := getter(run)) is not None
            ]
            rows.append(
                {
                    "metric": metric,
                    "observed_count": len(observed_values),
                    "eligible_count": len(eligible_values),
                    "failed_count": sum(run.verdict() == Verdict.FAIL for run in self.runs),
                    "incomplete_count": sum(
                        run.verdict() == Verdict.INCOMPLETE for run in self.runs
                    ),
                    "p50": nearest_rank(observed_values, 0.50),
                    "p95": nearest_rank(observed_values, 0.95),
                    "max": max(observed_values) if observed_values else None,
                    "clock_basis": clock_basis,
                    "percentile_method": "nearest-rank",
                    "percentile_population": "observed_with_metric",
                }
            )
        return rows

    def network_summary_rows(self) -> list[dict[str, object]]:
        """Суммирует request→headers/first-byte/body отдельно, не требуя body EOF."""

        rows: list[dict[str, object]] = []
        anomaly_summary = self.network_anomaly_summary()
        for metric, attribute in (
            ("request_to_headers_ms", "headers_ms"),
            ("request_to_first_byte_ms", "first_body_ms"),
            ("request_to_body_complete_ms", "body_complete_ms"),
        ):
            values = [
                value
                for request in self.network_requests
                if not request.ambiguous
                and (value := getattr(request, attribute)) is not None
            ]
            rows.append(
                {
                    "metric": metric,
                    "observed_count": len(values),
                    "request_count": len(self.network_requests),
                    "ambiguous_count": sum(
                        request.ambiguous for request in self.network_requests
                    ),
                    "anomaly_count": anomaly_summary["anomaly_count"],
                    "proof_relevant_anomaly_count": anomaly_summary[
                        "proof_relevant_anomaly_count"
                    ],
                    "p50": nearest_rank(values, 0.50),
                    "p95": nearest_rank(values, 0.95),
                    "max": max(values) if values else None,
                    "clock_basis": "request_owner_monotonic",
                    "percentile_method": "nearest-rank",
                }
            )
        return rows

    def network_anomaly_summary(self) -> dict[str, object]:
        """Суммирует typed terminal anomalies для JSON/table/strict consumers."""

        by_kind: dict[str, int] = {}
        for anomaly in self.network_terminal_anomalies:
            by_kind[anomaly.kind] = by_kind.get(anomaly.kind, 0) + 1
        return {
            "anomaly_count": len(self.network_terminal_anomalies),
            "proof_relevant_anomaly_count": sum(
                anomaly.proof_relevant()
                for anomaly in self.network_terminal_anomalies
            ),
            "by_kind": by_kind,
        }

    def scrub_anomaly_summary(self) -> dict[str, object]:
        """Суммирует command-correlation anomalies без подмены sample verdict-а."""

        by_kind: dict[str, int] = {}
        for anomaly in self.scrub_command_anomalies:
            by_kind[anomaly.kind] = by_kind.get(anomaly.kind, 0) + 1
        return {
            "anomaly_count": len(self.scrub_command_anomalies),
            "proof_relevant_anomaly_count": sum(
                anomaly.proof_relevant for anomaly in self.scrub_command_anomalies
            ),
            "by_kind": by_kind,
        }

    def hls_manifest_selection_summary_rows(self) -> list[dict[str, object]]:
        """Группирует HLS exact selections отдельно по cold/warm phase и role."""

        return hls_manifest_selection_summary_rows(self.hls_manifest_selections)

    def hls_manifest_selection_anomaly_summary(self) -> dict[str, object]:
        """Публикует typed HLS marker anomalies отдельным additive summary."""

        return hls_manifest_selection_anomaly_summary(
            self.hls_manifest_selection_anomalies
        )

    def has_proof_relevant_anomalies(self) -> bool:
        """Возвращает blocking telemetry corruption для strict acceptance."""

        return any(
            anomaly.proof_relevant()
            for anomaly in self.network_terminal_anomalies
        ) or any(
            anomaly.proof_relevant for anomaly in self.scrub_command_anomalies
        ) or any(
            anomaly.proof_relevant()
            for anomaly in self.hls_manifest_selection_anomalies
        )

    def to_dict(self) -> dict[str, object]:
        """Строит полный machine-readable acceptance report."""

        return {
            "scenario": self.scenario,
            "startup_runs": [run.to_dict() for run in self.runs],
            "seek_samples": [sample.to_dict() for sample in self.samples],
            "network_requests": [
                request.to_dict() for request in self.network_requests
            ],
            "network_terminal_anomalies": [
                anomaly.to_dict() for anomaly in self.network_terminal_anomalies
            ],
            "scrub_command_anomalies": [
                anomaly.to_dict() for anomaly in self.scrub_command_anomalies
            ],
            "hls_manifest_selections": [
                selection.to_dict() for selection in self.hls_manifest_selections
            ],
            "hls_manifest_selection_anomalies": [
                anomaly.to_dict()
                for anomaly in self.hls_manifest_selection_anomalies
            ],
            "startup_summary": self.startup_summary_rows(),
            "seek_summary": [row.to_dict() for row in self.summary_rows()],
            "network_summary": self.network_summary_rows(),
            "network_anomaly_summary": self.network_anomaly_summary(),
            "scrub_anomaly_summary": self.scrub_anomaly_summary(),
            "hls_manifest_selection_summary": (
                self.hls_manifest_selection_summary_rows()
            ),
            "hls_manifest_selection_anomaly_summary": (
                self.hls_manifest_selection_anomaly_summary()
            ),
            "production_marker_requirements": list(PRODUCTION_MARKER_REQUIREMENTS),
        }


def target_from_public_command(line: str) -> float | None:
    """Читает target из явного field либо compact MediaTime debug output."""

    explicit_target = float_field(line, "target_ms", "target_milliseconds")
    if explicit_target is not None:
        return explicit_target
    match = PUBLIC_COMMAND_TARGET_PATTERN.search(line)
    if match is None:
        return None
    value = float(match.group("value"))
    return value if match.group("unit") == "ms" else value * 1000.0


def scrub_begin_to_first_preview_ms(sample: SeekSample) -> float | None:
    """Извлекает typed scrub stage для generic summary builder."""

    return sample.scrub.begin_to_first_preview_ms if sample.scrub is not None else None


def scrub_begin_to_end_ms(sample: SeekSample) -> float | None:
    """Извлекает полный drag interval без wall-clock fallback."""

    return sample.scrub.begin_to_end_ms if sample.scrub is not None else None


def line_is_relevant(line: str) -> bool:
    """Отбрасывает миллионы unrelated debug lines до regex/timestamp parsing."""

    if line_is_scrub_command_marker(line):
        return True
    if HLS_MANIFEST_SEGMENT_SEEK_MARKER in line:
        return True
    if any(marker in line for marker in RELEVANT_MARKERS):
        return True
    if PUBLIC_COMMAND_MARKER not in line:
        return False
    return any(
        command in line
        for command in (
            "command=Seek(",
            "command=BeginScrub",
            "command=PreviewScrub",
            "command=UpdateScrub",
            "command=EndScrub",
        )
    )


def point_not_after(point: LogPoint | None, limit: LogPoint) -> bool:
    """Проверяет terminal request point относительно supersede boundary."""

    if point is None:
        return False
    if point.wall_timestamp_ms is not None and limit.wall_timestamp_ms is not None:
        return point.wall_timestamp_ms <= limit.wall_timestamp_ms
    return point.line_number <= limit.line_number


def receipt_subpath_field(
    line: str,
    *,
    explicit_name: str,
    public_span: float | None,
    legacy_names: tuple[str, ...],
) -> float | None:
    """Различает explicit dual-origin schema и legacy receipt-only elapsed field."""

    explicit_value = float_field(line, explicit_name)
    if explicit_value is not None:
        return explicit_value
    if public_span is not None:
        return None
    return float_field(line, *legacy_names)
