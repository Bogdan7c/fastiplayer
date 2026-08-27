"""Secret-safe HTTP request lifecycle correlation для acceptance analyzer."""

from __future__ import annotations

from dataclasses import dataclass, field

from playback_acceptance import (
    HTTP_BODY_COMPLETE_MARKER,
    HTTP_CANCELLED_MARKERS,
    HTTP_FIRST_BODY_MARKER,
    HTTP_HEADERS_MARKER,
    HTTP_REQUEST_MARKER,
    LogPoint,
    NetworkRequest,
    field_value,
    float_field,
    int_field,
    wall_elapsed,
)


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
    requests: list[NetworkRequest] = field(default_factory=list)
    active_requests: list[NetworkRequest] = field(default_factory=list)

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

        stage = network_stage(line)
        if stage is None:
            return NetworkObservation(handled=False)
        request = self._request_for_stage(point, line, stage)
        if request is None:
            return NetworkObservation(handled=True)
        elapsed_ms = float_field(line, "elapsed_milliseconds", "elapsed_ms")
        if stage == "headers":
            request.headers_ms = elapsed_ms
        elif stage == "first_body":
            request.first_body_ms = elapsed_ms
        elif stage == "body_complete":
            request.body_complete_ms = elapsed_ms
            request.body_complete_point = point
            request.body_bytes = int_field(line, "received_body_bytes")
            self._remove_active(request)
        else:
            request.cancelled_ms = elapsed_ms
            request.cancelled_point = point
            self._remove_active(request)
        return NetworkObservation(handled=True)

    def _request_for_stage(
        self,
        point: LogPoint,
        line: str,
        stage: str,
    ) -> NetworkRequest | None:
        """Выбирает request по ID либо уникальному wall-vs-owner elapsed совпадению."""

        operation_kind = field_value(line, "operation_kind") or ""
        candidates = [
            request
            for request in self.active_requests
            if request.operation_kind == operation_kind
            and network_stage_missing(request, stage)
        ]
        explicit_id = explicit_http_request_id(line)
        if explicit_id is not None:
            matching = [
                request
                for request in candidates
                if request.safe_request_id == explicit_id
            ]
            if len(matching) == 1:
                return matching[0]

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
        return None

    def _remove_active(self, request: NetworkRequest) -> None:
        """Terminal stage снимает только exact request identity."""

        if request in self.active_requests:
            self.active_requests.remove(request)


def explicit_http_request_id(line: str) -> str | None:
    """Принимает только transport-owned correlation IDs, не URL."""

    for key in ("http_request_id", "resource_request_id"):
        value = field_value(line, key)
        if value is not None:
            return value
    return None


def safe_http_request_id(line: str, anonymous_sequence: int) -> str:
    """Не включает request target в report и создаёт стабильный anonymous ID."""

    return explicit_http_request_id(line) or f"anonymous-{anonymous_sequence}"


def network_stage(line: str) -> str | None:
    """Классифицирует HTTP lifecycle marker."""

    if HTTP_HEADERS_MARKER in line:
        return "headers"
    if HTTP_FIRST_BODY_MARKER in line:
        return "first_body"
    if HTTP_BODY_COMPLETE_MARKER in line:
        return "body_complete"
    if any(marker in line for marker in HTTP_CANCELLED_MARKERS):
        return "cancelled"
    return None


def network_stage_missing(request: NetworkRequest, stage: str) -> bool:
    """Не позволяет повторному marker-у перезаписать первый stage."""

    if stage == "headers":
        return request.headers_ms is None
    if stage == "first_body":
        return request.first_body_ms is None
    if stage == "body_complete":
        return request.body_complete_ms is None
    return request.cancelled_ms is None


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
