"""Secret-safe HTTP request lifecycle correlation для acceptance analyzer."""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum

from playback_acceptance import (
    HTTP_BOUNDED_TERMINAL_MARKER,
    HTTP_BODY_COMPLETE_MARKER,
    HTTP_CANCELLED_MARKERS,
    HTTP_FIRST_BODY_MARKER,
    HTTP_HEADERS_MARKER,
    HTTP_REQUEST_MARKER,
    LogPoint,
    NetworkRequest,
    NetworkTerminalAnomaly,
    field_value,
    float_field,
    int_field,
    wall_elapsed,
)


class NetworkStage(str, Enum):
    """Typed lifecycle stage исключает смешение terminal outcomes."""

    HEADERS = "headers"
    FIRST_BODY = "first_body"
    BODY_COMPLETE = "body_complete"
    CANCELLED = "cancelled"
    ERROR = "error"
    REDIRECT = "redirect"


class NetworkTerminalAnomalyKind(str, Enum):
    """Fail-closed причины, по которым terminal marker не доказывает cancellation."""

    MISSING_OUTCOME = "missing_outcome"
    MISSING_REQUEST_ID = "missing_request_id"
    UNSUPPORTED_OUTCOME = "unsupported_outcome"
    UNKNOWN_REQUEST_ID = "unknown_request_id"
    INACTIVE_REQUEST_ID = "inactive_request_id"
    AMBIGUOUS_REQUEST_ID = "ambiguous_request_id"
    UNMATCHED_LEGACY_TERMINAL = "unmatched_legacy_terminal"


class NetworkCorrelationPolicy(str, Enum):
    """Current terminal требует ID; historical marker допускает old fallback."""

    EXACT_BOUNDED_TERMINAL = "exact_bounded_terminal"
    LEGACY_MARKER_FALLBACK = "legacy_marker_fallback"


class NetworkAnomalyImpact(str, Enum):
    """Typed strict impact без inference в CLI layer."""

    PROOF_RELEVANT = "proof_relevant"
    DIAGNOSTIC_ONLY = "diagnostic_only"


@dataclass(frozen=True)
class NetworkObservation:
    """Результат routing одной строки без двусмысленного optional bool API."""

    handled: bool
    started_request: NetworkRequest | None = None


@dataclass
class NetworkTracker:
    """Владеет active HTTP requests строго одного process source."""

    source: str
    first_sequence: int
    first_anomaly_sequence: int = 1
    requests: list[NetworkRequest] = field(default_factory=list)
    active_requests: list[NetworkRequest] = field(default_factory=list)
    terminal_anomalies: list[NetworkTerminalAnomaly] = field(default_factory=list)

    def observe(
        self,
        point: LogPoint,
        line: str,
        owner_seek_sequence: int | None,
    ) -> NetworkObservation:
        """Создаёт request либо применяет stage с fail-closed correlation."""

        if HTTP_REQUEST_MARKER in line:
            request = NetworkRequest(
                sequence=self.first_sequence + len(self.requests),
                source=self.source,
                safe_request_id=safe_http_request_id(line, len(self.requests) + 1),
                operation_kind=field_value(line, "operation_kind") or "",
                owner_seek_sequence=owner_seek_sequence,
                started=point,
            )
            self.requests.append(request)
            self.active_requests.append(request)
            return NetworkObservation(handled=True, started_request=request)

        if HTTP_BOUNDED_TERMINAL_MARKER in line:
            self._observe_bounded_terminal(point, line, owner_seek_sequence)
            return NetworkObservation(handled=True)

        stage = network_stage(line)
        if stage is None:
            return NetworkObservation(handled=False)
        request = self._request_for_stage(
            point,
            line,
            stage,
            NetworkCorrelationPolicy.LEGACY_MARKER_FALLBACK,
            owner_seek_sequence,
        )
        if request is None:
            return NetworkObservation(handled=True)
        self._apply_stage(request, point, line, stage)
        return NetworkObservation(handled=True)

    def _apply_stage(
        self,
        request: NetworkRequest,
        point: LogPoint,
        line: str,
        stage: NetworkStage,
    ) -> None:
        """Применяет уже correlated lifecycle stage ровно к одному request."""

        elapsed_ms = float_field(line, "elapsed_milliseconds", "elapsed_ms")
        if stage == NetworkStage.HEADERS:
            request.headers_ms = elapsed_ms
        elif stage == NetworkStage.FIRST_BODY:
            request.first_body_ms = elapsed_ms
        elif stage == NetworkStage.BODY_COMPLETE:
            request.body_complete_ms = elapsed_ms
            request.body_complete_point = point
            request.body_bytes = int_field(line, "received_body_bytes", "received_bytes")
            self._record_terminal(request, point, line, "complete", elapsed_ms)
            self._remove_active(request)
        elif stage == NetworkStage.CANCELLED:
            request.cancelled_ms = elapsed_ms
            request.cancelled_point = point
            request.body_bytes = int_field(line, "received_bytes", "received_body_bytes")
            self._record_terminal(request, point, line, "cancelled", elapsed_ms)
            self._remove_active(request)
        else:
            request.body_bytes = int_field(line, "received_bytes", "received_body_bytes")
            self._record_terminal(request, point, line, stage.value, elapsed_ms)
            self._remove_active(request)

    def _observe_bounded_terminal(
        self,
        point: LogPoint,
        line: str,
        owner_seek_sequence: int | None,
    ) -> None:
        """Current terminal marker никогда не использует legacy candidate fallback."""

        stage = bounded_terminal_stage(line)
        explicit_id = explicit_http_request_id(line)
        if explicit_id is None:
            candidates = self._legacy_candidates(line, stage)
            for request in candidates:
                request.ambiguous = True
            anomaly_owner = owner_for_anomaly(owner_seek_sequence, candidates)
            self._record_terminal_anomaly(
                point,
                line,
                NetworkTerminalAnomalyKind.MISSING_REQUEST_ID,
                anomaly_owner,
                NetworkAnomalyImpact.PROOF_RELEVANT,
            )
            if stage is None:
                self._record_invalid_outcome_anomaly(
                    point,
                    line,
                    anomaly_owner,
                )
            return

        if stage is None:
            self._observe_invalid_bounded_terminal(
                point,
                line,
                owner_seek_sequence,
            )
            return
        request = self._request_for_stage(
            point,
            line,
            stage,
            NetworkCorrelationPolicy.EXACT_BOUNDED_TERMINAL,
            owner_seek_sequence,
        )
        if request is not None:
            self._apply_stage(request, point, line, stage)

    def _request_for_stage(
        self,
        point: LogPoint,
        line: str,
        stage: NetworkStage,
        correlation_policy: NetworkCorrelationPolicy,
        owner_seek_sequence: int | None,
    ) -> NetworkRequest | None:
        """Выбирает exact ID; fallback доступен только historical marker family."""

        explicit_id = explicit_http_request_id(line)
        if explicit_id is not None:
            matching = [
                request
                for request in self.active_requests
                if request.safe_request_id == explicit_id
                and network_stage_missing(request, stage)
            ]
            if len(matching) == 1:
                return matching[0]
            exact_active_requests = [
                request
                for request in self.active_requests
                if request.safe_request_id == explicit_id
            ]
            for request in exact_active_requests:
                request.ambiguous = True
            if is_terminal_stage(stage):
                self._record_request_id_anomaly(
                    point,
                    line,
                    explicit_id,
                    exact_active_requests,
                )
            return None

        if correlation_policy == NetworkCorrelationPolicy.EXACT_BOUNDED_TERMINAL:
            self._record_terminal_anomaly(
                point,
                line,
                NetworkTerminalAnomalyKind.MISSING_REQUEST_ID,
                owner_seek_sequence,
                NetworkAnomalyImpact.PROOF_RELEVANT,
            )
            return None

        candidates = self._legacy_candidates(line, stage)

        elapsed_ms = float_field(line, "elapsed_milliseconds", "elapsed_ms")
        timed_matches = [
            request
            for request in candidates
            if network_elapsed_matches(request, point, elapsed_ms)
        ]
        if len(timed_matches) == 1:
            return timed_matches[0]
        if len(candidates) == 1:
            return candidates[0]
        for request in timed_matches or candidates:
            request.ambiguous = True
        if is_terminal_stage(stage):
            self._record_terminal_anomaly(
                point,
                line,
                NetworkTerminalAnomalyKind.UNMATCHED_LEGACY_TERMINAL,
                owner_for_anomaly(owner_seek_sequence, timed_matches or candidates),
                (
                    NetworkAnomalyImpact.PROOF_RELEVANT
                    if owner_seek_sequence is not None or timed_matches or candidates
                    else NetworkAnomalyImpact.DIAGNOSTIC_ONLY
                ),
            )
        return None

    def _legacy_candidates(
        self,
        line: str,
        stage: NetworkStage | None,
    ) -> list[NetworkRequest]:
        """Строит candidates только для explicit historical compatibility path."""

        operation_kind = field_value(line, "operation_kind")
        return [
            request
            for request in self.active_requests
            if (operation_kind is None or request.operation_kind == operation_kind)
            and (stage is None or network_stage_missing(request, stage))
        ]

    def _observe_invalid_bounded_terminal(
        self,
        point: LogPoint,
        line: str,
        owner_seek_sequence: int | None,
    ) -> None:
        """Публикует invalid outcome и закрывает exact request без ложного результата."""

        explicit_id = explicit_http_request_id(line)
        exact_active_requests = [
            request
            for request in self.active_requests
            if request.safe_request_id == explicit_id
        ]
        anomaly_owner = owner_for_anomaly(owner_seek_sequence, exact_active_requests)
        self._record_invalid_outcome_anomaly(point, line, anomaly_owner)
        if len(exact_active_requests) != 1:
            for request in exact_active_requests:
                request.ambiguous = True
            self._record_request_id_anomaly(
                point,
                line,
                explicit_id,
                exact_active_requests,
            )
            return

        request = exact_active_requests[0]
        outcome = field_value(line, "outcome")
        request.body_bytes = int_field(line, "received_bytes", "received_body_bytes")
        self._record_terminal(
            request,
            point,
            line,
            outcome or "",
            float_field(line, "elapsed_milliseconds", "elapsed_ms"),
        )
        request.ambiguous = True
        self._remove_active(request)

    def _record_invalid_outcome_anomaly(
        self,
        point: LogPoint,
        line: str,
        owner_seek_sequence: int | None,
    ) -> None:
        """Различает missing и unsupported typed terminal outcomes."""

        outcome = field_value(line, "outcome")
        anomaly_kind = (
            NetworkTerminalAnomalyKind.MISSING_OUTCOME
            if outcome is None
            else NetworkTerminalAnomalyKind.UNSUPPORTED_OUTCOME
        )
        self._record_terminal_anomaly(
            point,
            line,
            anomaly_kind,
            owner_seek_sequence,
            NetworkAnomalyImpact.PROOF_RELEVANT,
        )

    def _record_request_id_anomaly(
        self,
        point: LogPoint,
        line: str,
        explicit_id: str,
        exact_active_requests: list[NetworkRequest],
    ) -> None:
        """Различает неизвестный, уже закрытый и неоднозначный exact request ID."""

        if len(exact_active_requests) > 1:
            kind = NetworkTerminalAnomalyKind.AMBIGUOUS_REQUEST_ID
        elif any(request.safe_request_id == explicit_id for request in self.requests):
            kind = NetworkTerminalAnomalyKind.INACTIVE_REQUEST_ID
        else:
            kind = NetworkTerminalAnomalyKind.UNKNOWN_REQUEST_ID
        historical_requests = [
            request
            for request in self.requests
            if request.safe_request_id == explicit_id
        ]
        self._record_terminal_anomaly(
            point,
            line,
            kind,
            owner_for_anomaly(None, historical_requests),
            NetworkAnomalyImpact.PROOF_RELEVANT,
        )

    def _record_terminal_anomaly(
        self,
        point: LogPoint,
        line: str,
        kind: NetworkTerminalAnomalyKind,
        owner_seek_sequence: int | None,
        impact: NetworkAnomalyImpact,
    ) -> None:
        """Сохраняет terminal anomaly отдельно от request lifecycle evidence."""

        self.terminal_anomalies.append(
            NetworkTerminalAnomaly(
                sequence=self.first_anomaly_sequence + len(self.terminal_anomalies),
                source=self.source,
                kind=kind.value,
                safe_request_id=explicit_http_request_id(line),
                outcome=field_value(line, "outcome"),
                elapsed_ms=float_field(line, "elapsed_milliseconds", "elapsed_ms"),
                received_bytes=int_field(line, "received_bytes", "received_body_bytes"),
                line_number=point.line_number,
                owner_seek_sequence=owner_seek_sequence,
                impact=impact.value,
            )
        )

    @staticmethod
    def _record_terminal(
        request: NetworkRequest,
        point: LogPoint,
        line: str,
        fallback_outcome: str,
        elapsed_ms: float | None,
    ) -> None:
        """Сохраняет общий typed terminal без подмены error/redirect cancellation-ом."""

        request.terminal_outcome = field_value(line, "outcome") or fallback_outcome
        request.terminal_ms = elapsed_ms
        request.terminal_point = point
        request.terminal_error_category = field_value(line, "error_category") or ""

    def _remove_active(self, request: NetworkRequest) -> None:
        """Terminal stage снимает только exact request identity."""

        if request in self.active_requests:
            self.active_requests.remove(request)


def owner_for_anomaly(
    current_owner_seek_sequence: int | None,
    candidate_requests: list[NetworkRequest],
) -> int | None:
    """Выбирает exact current owner либо единственного owner-а candidates."""

    candidate_owners = {
        request.owner_seek_sequence
        for request in candidate_requests
        if request.owner_seek_sequence is not None
    }
    if len(candidate_owners) == 1:
        return next(iter(candidate_owners))
    if candidate_requests:
        return None
    return current_owner_seek_sequence


def explicit_http_request_id(line: str) -> str | None:
    """Принимает только transport-owned correlation IDs, не URL."""

    for key in ("http_request_id", "resource_request_id", "request_id"):
        value = field_value(line, key)
        if value is not None:
            return value
    return None


def safe_http_request_id(line: str, anonymous_sequence: int) -> str:
    """Не включает request target в report и создаёт стабильный anonymous ID."""

    return explicit_http_request_id(line) or f"anonymous-{anonymous_sequence}"


def network_stage(line: str) -> NetworkStage | None:
    """Классифицирует HTTP lifecycle marker."""

    if HTTP_HEADERS_MARKER in line:
        return NetworkStage.HEADERS
    if HTTP_FIRST_BODY_MARKER in line:
        return NetworkStage.FIRST_BODY
    if HTTP_BODY_COMPLETE_MARKER in line:
        return NetworkStage.BODY_COMPLETE
    if any(marker in line for marker in HTTP_CANCELLED_MARKERS):
        return NetworkStage.CANCELLED
    return None


def bounded_terminal_stage(line: str) -> NetworkStage | None:
    """Принимает только outcomes, семантика которых закреплена report schema."""

    outcome = field_value(line, "outcome")
    return {
        "cancelled": NetworkStage.CANCELLED,
        "complete": NetworkStage.BODY_COMPLETE,
        "completed": NetworkStage.BODY_COMPLETE,
        "error": NetworkStage.ERROR,
        "redirect": NetworkStage.REDIRECT,
    }.get(outcome)


def is_terminal_stage(stage: NetworkStage) -> bool:
    """Отделяет terminal lifecycle stages от headers/first-body progress."""

    return stage in {
        NetworkStage.BODY_COMPLETE,
        NetworkStage.CANCELLED,
        NetworkStage.ERROR,
        NetworkStage.REDIRECT,
    }


def network_stage_missing(request: NetworkRequest, stage: NetworkStage) -> bool:
    """Не позволяет повторному marker-у перезаписать первый stage."""

    if stage == NetworkStage.HEADERS:
        return request.headers_ms is None
    if stage == NetworkStage.FIRST_BODY:
        return request.first_body_ms is None
    if stage == NetworkStage.BODY_COMPLETE:
        return request.body_complete_ms is None
    if stage == NetworkStage.CANCELLED:
        return request.cancelled_ms is None
    return request.terminal_outcome == ""


def network_elapsed_matches(
    request: NetworkRequest,
    point: LogPoint,
    owner_elapsed_ms: float | None,
    tolerance_ms: float = 20.0,
) -> bool:
    """Использует wall time только для correlation, не для итоговой latency."""

    if owner_elapsed_ms is None:
        return False
    observed_wall_ms = wall_elapsed(request.started, point)
    if observed_wall_ms is None:
        return False
    return abs(observed_wall_ms - owner_elapsed_ms) <= tolerance_ms
