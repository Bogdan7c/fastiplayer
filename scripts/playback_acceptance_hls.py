"""Exact HLS manifest-selection evidence для offline playback acceptance.

Модуль намеренно не знает о public seek/scrub operations. HLS marker имеет
собственную process-local identity и остаётся независимым доказательством
выбранного manifest segment-а и packet-derived landing anchor-а.
"""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from enum import Enum


HLS_MANIFEST_SEGMENT_SEEK_MARKER = "kind=hls_manifest_segment_seek"
U64_MAX = (1 << 64) - 1


class HlsManifestSelectionPhase(str, Enum):
    """Точные phase values production formatter-а."""

    INITIAL_OPEN = "initial_open"
    INITIAL_RESTORE = "initial_restore"
    PREVIEW = "preview"
    FINAL_RECEIPT = "final_receipt"


class HlsManifestComponentRole(str, Enum):
    """Безопасная HLS topology role без locator material."""

    MUXED = "muxed"
    VIDEO = "video"
    AUDIO = "audio"


class HlsManifestLandingPolicy(str, Enum):
    """Поддержанные landing policy production formatter-а."""

    DECODE_FROM_OR_BEFORE_TARGET = "decode_from_or_before_target"
    PREFER_POST_TARGET_RAP = "prefer_post_target_rap"


class HlsManifestAnchorKind(str, Enum):
    """Packet-derived anchor kind, а не эвристика parser-а."""

    VIDEO_RANDOM_ACCESS_POINT = "video_random_access_point"
    AUDIO_PACKET = "audio_packet"


class HlsManifestSelectionAnomalyKind(str, Enum):
    """Fail-closed причины, по которым marker нельзя считать чистым evidence."""

    MISSING_FIELD = "missing_field"
    DUPLICATE_FIELD = "duplicate_field"
    INVALID_UNSIGNED_DECIMAL = "invalid_unsigned_decimal"
    UNSIGNED_DECIMAL_OVERFLOW = "unsigned_decimal_overflow"
    ZERO_SELECTION_ID = "zero_selection_id"
    UNKNOWN_PHASE = "unknown_phase"
    UNKNOWN_COMPONENT_ROLE = "unknown_component_role"
    UNKNOWN_LANDING_POLICY = "unknown_landing_policy"
    UNKNOWN_ANCHOR_KIND = "unknown_anchor_kind"
    DUPLICATE_SELECTION_ID = "duplicate_selection_id"
    INVALID_SEGMENT_INTERVAL = "invalid_segment_interval"
    ACTUAL_ANCHOR_OUTSIDE_SEGMENT = "actual_anchor_outside_segment"
    ANCHOR_KIND_ROLE_MISMATCH = "anchor_kind_role_mismatch"


@dataclass(frozen=True)
class HlsManifestSelectionRecord:
    """Одна exact HLS selection без придуманной public-operation correlation."""

    sequence: int
    source: str
    line_number: int
    phase: str
    component_role: str
    manifest_selection_id: int
    landing_policy: str
    source_generation: int
    requested_target_ms: int
    actual_anchor_ms: int
    actual_decode_anchor_ms: int
    anchor_kind: str
    media_sequence: int
    discontinuity_sequence: int
    manifest_segment_index: int
    epoch_index: int
    restart_segment_index: int
    segment_start_ms: int
    segment_end_ms: int
    anomaly_kinds: tuple[str, ...] = ()

    def operation_class(self) -> str:
        """Разделяет cold и warm только по HLS-owned phase."""

        if self.phase in {
            HlsManifestSelectionPhase.INITIAL_OPEN.value,
            HlsManifestSelectionPhase.INITIAL_RESTORE.value,
        }:
            return "cold"
        return "warm"

    def valid(self) -> bool:
        """Marker eligible только при отсутствии typed validation anomalies."""

        return not self.anomaly_kinds

    def to_dict(self) -> dict[str, object]:
        """Сериализует только formatter-owned scalar evidence."""

        return {
            "sequence": self.sequence,
            "source": self.source,
            "line_number": self.line_number,
            "operation_class": self.operation_class(),
            "phase": self.phase,
            "component_role": self.component_role,
            "manifest_selection_id": self.manifest_selection_id,
            "landing_policy": self.landing_policy,
            "source_generation": self.source_generation,
            "requested_target_ms": self.requested_target_ms,
            "actual_anchor_ms": self.actual_anchor_ms,
            "actual_decode_anchor_ms": self.actual_decode_anchor_ms,
            "anchor_kind": self.anchor_kind,
            "media_sequence": self.media_sequence,
            "discontinuity_sequence": self.discontinuity_sequence,
            "manifest_segment_index": self.manifest_segment_index,
            "epoch_index": self.epoch_index,
            "restart_segment_index": self.restart_segment_index,
            "segment_start_ms": self.segment_start_ms,
            "segment_end_ms": self.segment_end_ms,
            "anomaly_kinds": list(self.anomaly_kinds),
            "valid": self.valid(),
        }


@dataclass(frozen=True)
class HlsManifestSelectionAnomaly:
    """Secret-safe ошибка HLS marker schema или semantic consistency."""

    sequence: int
    source: str
    line_number: int
    kind: str
    field: str | None
    record_sequence: int | None
    component_role: str | None
    manifest_selection_id: int | None
    impact: str = "proof_relevant"

    def proof_relevant(self) -> bool:
        """HLS marker corruption всегда блокирует strict evidence."""

        return self.impact == "proof_relevant"

    def to_dict(self) -> dict[str, object]:
        """Не переносит raw line, malformed value, URL или token в report."""

        return {
            "sequence": self.sequence,
            "source": self.source,
            "line_number": self.line_number,
            "anomaly_kind": self.kind,
            "field": self.field,
            "record_sequence": self.record_sequence,
            "component_role": self.component_role,
            "manifest_selection_id": self.manifest_selection_id,
            "impact": self.impact,
            "proof_relevant": self.proof_relevant(),
        }


@dataclass(frozen=True)
class _FieldFailure:
    """Внутренний parse failure без сохранения потенциально секретного value."""

    kind: HlsManifestSelectionAnomalyKind
    field: str


@dataclass
class HlsManifestSelectionTracker:
    """Владеет opaque ID uniqueness и validation одного log source-а."""

    source: str
    first_record_sequence: int = 1
    first_anomaly_sequence: int = 1
    records: list[HlsManifestSelectionRecord] = field(default_factory=list)
    anomalies: list[HlsManifestSelectionAnomaly] = field(default_factory=list)
    _seen_selection_ids: set[int] = field(default_factory=set)

    def observe(self, line_number: int, line: str) -> bool:
        """Парсит только exact HLS marker и возвращает факт обработки строки."""

        if HLS_MANIFEST_SEGMENT_SEEK_MARKER not in line:
            return False

        enum_values, unsigned_values, failures = _parse_marker_fields(line)
        component_role = enum_values.get("component_role")
        manifest_selection_id = unsigned_values.get("manifest_selection_id")
        if failures:
            for failure in failures:
                self._append_anomaly(
                    line_number=line_number,
                    kind=failure.kind,
                    field_name=failure.field,
                    record_sequence=None,
                    component_role=component_role,
                    manifest_selection_id=manifest_selection_id,
                )
            return True

        record_sequence = self.first_record_sequence + len(self.records)
        semantic_failures = self._semantic_failures(enum_values, unsigned_values)
        for failure in semantic_failures:
            self._append_anomaly(
                line_number=line_number,
                kind=failure.kind,
                field_name=failure.field,
                record_sequence=record_sequence,
                component_role=component_role,
                manifest_selection_id=manifest_selection_id,
            )

        self.records.append(
            HlsManifestSelectionRecord(
                sequence=record_sequence,
                source=self.source,
                line_number=line_number,
                phase=enum_values["phase"],
                component_role=enum_values["component_role"],
                manifest_selection_id=unsigned_values["manifest_selection_id"],
                landing_policy=enum_values["landing_policy"],
                source_generation=unsigned_values["source_generation"],
                requested_target_ms=unsigned_values["requested_target_ms"],
                actual_anchor_ms=unsigned_values["actual_anchor_ms"],
                actual_decode_anchor_ms=unsigned_values[
                    "actual_decode_anchor_ms"
                ],
                anchor_kind=enum_values["anchor_kind"],
                media_sequence=unsigned_values["media_sequence"],
                discontinuity_sequence=unsigned_values[
                    "discontinuity_sequence"
                ],
                manifest_segment_index=unsigned_values[
                    "manifest_segment_index"
                ],
                epoch_index=unsigned_values["epoch_index"],
                restart_segment_index=unsigned_values["restart_segment_index"],
                segment_start_ms=unsigned_values["segment_start_ms"],
                segment_end_ms=unsigned_values["segment_end_ms"],
                anomaly_kinds=tuple(failure.kind.value for failure in semantic_failures),
            )
        )
        return True

    def _semantic_failures(
        self,
        enum_values: dict[str, str],
        unsigned_values: dict[str, int],
    ) -> list[_FieldFailure]:
        """Проверяет только гарантированные formatter/anchor invariants."""

        failures: list[_FieldFailure] = []
        role = enum_values["component_role"]
        selection_id = unsigned_values["manifest_selection_id"]
        if selection_id == 0:
            failures.append(
                _FieldFailure(
                    HlsManifestSelectionAnomalyKind.ZERO_SELECTION_ID,
                    "manifest_selection_id",
                )
            )

        if selection_id in self._seen_selection_ids:
            failures.append(
                _FieldFailure(
                    HlsManifestSelectionAnomalyKind.DUPLICATE_SELECTION_ID,
                    "manifest_selection_id",
                )
            )
        self._seen_selection_ids.add(selection_id)

        segment_start_ms = unsigned_values["segment_start_ms"]
        segment_end_ms = unsigned_values["segment_end_ms"]
        actual_anchor_ms = unsigned_values["actual_anchor_ms"]
        if segment_end_ms <= segment_start_ms:
            failures.append(
                _FieldFailure(
                    HlsManifestSelectionAnomalyKind.INVALID_SEGMENT_INTERVAL,
                    "segment_end_ms",
                )
            )
        elif not segment_start_ms <= actual_anchor_ms < segment_end_ms:
            failures.append(
                _FieldFailure(
                    HlsManifestSelectionAnomalyKind.ACTUAL_ANCHOR_OUTSIDE_SEGMENT,
                    "actual_anchor_ms",
                )
            )

        anchor_kind = enum_values["anchor_kind"]
        if (
            role == HlsManifestComponentRole.VIDEO.value
            and anchor_kind
            != HlsManifestAnchorKind.VIDEO_RANDOM_ACCESS_POINT.value
        ) or (
            role == HlsManifestComponentRole.AUDIO.value
            and anchor_kind != HlsManifestAnchorKind.AUDIO_PACKET.value
        ):
            failures.append(
                _FieldFailure(
                    HlsManifestSelectionAnomalyKind.ANCHOR_KIND_ROLE_MISMATCH,
                    "anchor_kind",
                )
            )

        return failures

    def _append_anomaly(
        self,
        *,
        line_number: int,
        kind: HlsManifestSelectionAnomalyKind,
        field_name: str | None,
        record_sequence: int | None,
        component_role: str | None,
        manifest_selection_id: int | None,
    ) -> None:
        """Добавляет anomaly с analyzer-global sequence без raw marker content."""

        self.anomalies.append(
            HlsManifestSelectionAnomaly(
                sequence=self.first_anomaly_sequence + len(self.anomalies),
                source=self.source,
                line_number=line_number,
                kind=kind.value,
                field=field_name,
                record_sequence=record_sequence,
                component_role=component_role,
                manifest_selection_id=manifest_selection_id,
            )
        )


ENUM_FIELDS: dict[str, type[Enum]] = {
    "phase": HlsManifestSelectionPhase,
    "component_role": HlsManifestComponentRole,
    "landing_policy": HlsManifestLandingPolicy,
    "anchor_kind": HlsManifestAnchorKind,
}

UNKNOWN_ENUM_ANOMALIES = {
    "phase": HlsManifestSelectionAnomalyKind.UNKNOWN_PHASE,
    "component_role": HlsManifestSelectionAnomalyKind.UNKNOWN_COMPONENT_ROLE,
    "landing_policy": HlsManifestSelectionAnomalyKind.UNKNOWN_LANDING_POLICY,
    "anchor_kind": HlsManifestSelectionAnomalyKind.UNKNOWN_ANCHOR_KIND,
}

UNSIGNED_FIELDS = (
    "manifest_selection_id",
    "source_generation",
    "requested_target_ms",
    "actual_anchor_ms",
    "actual_decode_anchor_ms",
    "media_sequence",
    "discontinuity_sequence",
    "manifest_segment_index",
    "epoch_index",
    "restart_segment_index",
    "segment_start_ms",
    "segment_end_ms",
)


def _parse_marker_fields(
    line: str,
) -> tuple[dict[str, str], dict[str, int], list[_FieldFailure]]:
    """Читает whitelisted exact fields; неизвестные suffix fields не попадают в report."""

    enum_values: dict[str, str] = {}
    unsigned_values: dict[str, int] = {}
    failures: list[_FieldFailure] = []

    for field_name, enum_type in ENUM_FIELDS.items():
        token, token_failure = _single_field_token(line, field_name)
        if token_failure is not None:
            failures.append(token_failure)
            continue
        try:
            enum_values[field_name] = enum_type(token).value
        except ValueError:
            failures.append(
                _FieldFailure(UNKNOWN_ENUM_ANOMALIES[field_name], field_name)
            )

    for field_name in UNSIGNED_FIELDS:
        token, token_failure = _single_field_token(line, field_name)
        if token_failure is not None:
            failures.append(token_failure)
            continue
        parsed_value, decimal_failure = _strict_u64(token, field_name)
        if decimal_failure is not None:
            failures.append(decimal_failure)
            continue
        unsigned_values[field_name] = parsed_value

    return enum_values, unsigned_values, failures


def _single_field_token(
    line: str, field_name: str
) -> tuple[str, _FieldFailure | None]:
    """Требует ровно один unquoted whitespace-delimited formatter token."""

    matches = re.findall(
        rf"(?<![A-Za-z0-9_]){re.escape(field_name)}\s*=\s*([^\s,]+)",
        line,
    )
    if not matches:
        return "", _FieldFailure(
            HlsManifestSelectionAnomalyKind.MISSING_FIELD,
            field_name,
        )
    if len(matches) != 1:
        return "", _FieldFailure(
            HlsManifestSelectionAnomalyKind.DUPLICATE_FIELD,
            field_name,
        )
    return matches[0], None


def _strict_u64(
    token: str, field_name: str
) -> tuple[int, _FieldFailure | None]:
    """Парсит ASCII decimal u64 без float, exponent и huge-int exceptions."""

    if re.fullmatch(r"[0-9]+", token) is None:
        return 0, _FieldFailure(
            HlsManifestSelectionAnomalyKind.INVALID_UNSIGNED_DECIMAL,
            field_name,
        )
    normalized = token.lstrip("0") or "0"
    maximum = str(U64_MAX)
    if len(normalized) > len(maximum) or (
        len(normalized) == len(maximum) and normalized > maximum
    ):
        return 0, _FieldFailure(
            HlsManifestSelectionAnomalyKind.UNSIGNED_DECIMAL_OVERFLOW,
            field_name,
        )
    return int(normalized), None


def hls_manifest_selection_summary_rows(
    records: list[HlsManifestSelectionRecord],
) -> list[dict[str, object]]:
    """Группирует exact rows по cold/warm phase и component role."""

    grouped: dict[tuple[str, str, str], list[HlsManifestSelectionRecord]] = {}
    for record in records:
        key = (record.operation_class(), record.phase, record.component_role)
        grouped.setdefault(key, []).append(record)

    phase_order = {
        phase.value: index for index, phase in enumerate(HlsManifestSelectionPhase)
    }
    role_order = {
        role.value: index for index, role in enumerate(HlsManifestComponentRole)
    }
    rows: list[dict[str, object]] = []
    for operation_class, phase, component_role in sorted(
        grouped,
        key=lambda key: (
            phase_order[key[1]],
            role_order[key[2]],
        ),
    ):
        grouped_records = grouped[(operation_class, phase, component_role)]
        rows.append(
            {
                "operation_class": operation_class,
                "phase": phase,
                "component_role": component_role,
                "selection_count": len(grouped_records),
                "valid_count": sum(record.valid() for record in grouped_records),
                "anomaly_count": sum(
                    len(record.anomaly_kinds) for record in grouped_records
                ),
            }
        )
    return rows


def hls_manifest_selection_anomaly_summary(
    anomalies: list[HlsManifestSelectionAnomaly],
) -> dict[str, object]:
    """Суммирует HLS schema/semantic anomalies для JSON/table/strict."""

    by_kind: dict[str, int] = {}
    for anomaly in anomalies:
        by_kind[anomaly.kind] = by_kind.get(anomaly.kind, 0) + 1
    return {
        "anomaly_count": len(anomalies),
        "proof_relevant_anomaly_count": sum(
            anomaly.proof_relevant() for anomaly in anomalies
        ),
        "by_kind": by_kind,
    }
