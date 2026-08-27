"""Exact scrub command correlation для offline playback acceptance analyzer."""

from __future__ import annotations

import re
from dataclasses import dataclass, field
from enum import Enum

from playback_acceptance import LogPoint, field_value, float_field


SCRUB_COMMAND_SCHEMA_VERSION = 1
U64_MAX = (1 << 64) - 1
U128_MAX = (1 << 128) - 1
MODERN_SCRUB_MARKER = "Player scrub command received"
LEGACY_PUBLIC_COMMAND_MARKER = "Player command received command="
LEGACY_TARGET_PATTERN = re.compile(
    r"target: (?P<kind>Absolute|Relative)\((?:MediaTime\()?"
    r"(?P<value>[0-9.]+)(?P<unit>ms|s)"
)


class PublicCommandForm(str, Enum):
    """Две tracing forms одной public scrub command."""

    DISPATCH = "dispatch"
    ACCEPTANCE = "acceptance"


MODERN_FORM_ALIASES = {
    PublicCommandForm.DISPATCH.value: PublicCommandForm.DISPATCH,
    PublicCommandForm.ACCEPTANCE.value: PublicCommandForm.ACCEPTANCE,
    "raw_debug": PublicCommandForm.DISPATCH,
    "seek_acceptance": PublicCommandForm.ACCEPTANCE,
}


class UnsignedDecimalFailure(str, Enum):
    """Причина отказа strict decimal field без float conversion."""

    MISSING = "missing"
    INVALID = "invalid"
    OVERFLOW = "overflow"


@dataclass(frozen=True)
class UnsignedDecimalField:
    """Exact unsigned integer либо typed parse failure."""

    value: int | None
    failure: UnsignedDecimalFailure | None


class ScrubCommandStage(str, Enum):
    """Semantic scrub stage без зависимости от Debug layout Rust enum-а."""

    BEGIN = "begin"
    UPDATE = "update"
    PREVIEW = "preview"
    END = "end"


class ScrubCorrelationAction(str, Enum):
    """Явное решение tracker-а для stateful parser-а."""

    APPLY_LOGICAL_COMMAND = "apply_logical_command"
    ENRICH_EXISTING = "enrich_existing"
    IGNORE_ANOMALOUS = "ignore_anomalous"


@dataclass(frozen=True)
class ScrubTargetIdentity:
    """Requested target identity, одинаковая до session target resolution."""

    kind: str
    milliseconds: int


@dataclass(frozen=True)
class ScrubCommandMarker:
    """Одна parsed INFO correlation form либо single-form legacy command."""

    point: LogPoint
    form: PublicCommandForm
    stage: ScrubCommandStage | None
    command_id: int | None
    target: ScrubTargetIdentity | None
    modern_schema: bool
    parse_failure: str | None = None


@dataclass(frozen=True)
class ScrubCommandDecision:
    """Решение correlation без optional bool с неясной семантикой."""

    action: ScrubCorrelationAction
    marker: ScrubCommandMarker
    correlation_key: str | None


@dataclass(frozen=True)
class ScrubCommandAnomaly:
    """Fail-closed evidence двусмысленной или повреждённой command telemetry."""

    sequence: int
    source: str
    kind: str
    line_number: int
    command_id: int | None
    form: str | None
    stage: str | None
    target_kind: str | None
    target_ms: int | None
    correlation_key: str | None = field(compare=False, repr=False)
    proof_relevant: bool = True

    def to_dict(self) -> dict[str, object]:
        """Сериализует secret-safe anomaly без raw command payload."""

        return {
            "sequence": self.sequence,
            "source": self.source,
            "anomaly_kind": self.kind,
            "line_number": self.line_number,
            "scrub_command_id": self.command_id,
            "scrub_command_form": self.form,
            "scrub_stage": self.stage,
            "scrub_target_kind": self.target_kind,
            "scrub_requested_target_ms": self.target_ms,
            "proof_relevant": self.proof_relevant,
        }


@dataclass
class _ModernCommandRecord:
    """Ожидаемые две forms одного exact modern ID."""

    key: str
    stage: ScrubCommandStage
    target: ScrubTargetIdentity
    forms: dict[PublicCommandForm, ScrubCommandMarker] = field(default_factory=dict)


@dataclass
class ScrubCommandCorrelationTracker:
    """Владеет exact-ID и explicit legacy compatibility policy одного source."""

    source: str
    first_anomaly_sequence: int = 1
    anomalies: list[ScrubCommandAnomaly] = field(default_factory=list)
    _modern_records: dict[int, _ModernCommandRecord] = field(default_factory=dict)
    _legacy_keys: list[str] = field(default_factory=list)
    _legacy_forms: set[PublicCommandForm] = field(default_factory=set)
    _legacy_primary_form: PublicCommandForm | None = None
    _last_modern_id: int | None = None
    _modern_seen: bool = False
    _legacy_seen: bool = False
    _next_legacy_sequence: int = 1
    _schema_mix_reported: bool = False
    _legacy_mix_reported: bool = False

    def observe(self, marker: ScrubCommandMarker) -> ScrubCommandDecision:
        """Коррелирует marker exact ID либо применяет single-form legacy policy."""

        if marker.parse_failure is not None:
            self._record_anomaly(marker.parse_failure, marker, None)
            return ScrubCommandDecision(
                ScrubCorrelationAction.IGNORE_ANOMALOUS,
                marker,
                None,
            )
        if marker.modern_schema:
            return self._observe_modern(marker)
        return self._observe_legacy(marker)

    def finish(self) -> list[ScrubCommandAnomaly]:
        """На EOF превращает каждую неполную modern pair в explicit anomaly."""

        for record in self._modern_records.values():
            if PublicCommandForm.DISPATCH not in record.forms:
                marker = next(iter(record.forms.values()))
                self._record_anomaly("missing_dispatch_form", marker, record.key)
            if PublicCommandForm.ACCEPTANCE not in record.forms:
                marker = next(iter(record.forms.values()))
                self._record_anomaly("missing_acceptance_form", marker, record.key)
        return self.anomalies

    def _observe_modern(
        self, marker: ScrubCommandMarker
    ) -> ScrubCommandDecision:
        """Использует только ID+stage+target, без time/adjacency inference."""

        self._modern_seen = True
        if self._legacy_seen:
            self._record_schema_mix(marker)
        if marker.command_id is None or marker.stage is None or marker.target is None:
            self._record_anomaly("incomplete_modern_identity", marker, None)
            return ScrubCommandDecision(
                ScrubCorrelationAction.IGNORE_ANOMALOUS,
                marker,
                None,
            )

        key = f"modern:{marker.command_id}"
        record = self._modern_records.get(marker.command_id)
        if record is None:
            record = _ModernCommandRecord(
                key=key,
                stage=marker.stage,
                target=marker.target,
            )
            self._modern_records[marker.command_id] = record
            if (
                self._last_modern_id is not None
                and marker.command_id <= self._last_modern_id
            ):
                self._record_anomaly("non_monotonic_scrub_command_id", marker, key)
            self._last_modern_id = max(
                marker.command_id,
                self._last_modern_id or marker.command_id,
            )
            record.forms[marker.form] = marker
            return ScrubCommandDecision(
                ScrubCorrelationAction.APPLY_LOGICAL_COMMAND,
                marker,
                key,
            )

        if marker.stage != record.stage:
            self._record_anomaly("scrub_stage_mismatch", marker, record.key)
            return ScrubCommandDecision(
                ScrubCorrelationAction.IGNORE_ANOMALOUS,
                marker,
                record.key,
            )
        if marker.target != record.target:
            self._record_anomaly("scrub_target_mismatch", marker, record.key)
            return ScrubCommandDecision(
                ScrubCorrelationAction.IGNORE_ANOMALOUS,
                marker,
                record.key,
            )
        if marker.form in record.forms:
            self._record_anomaly("duplicate_scrub_command_form", marker, record.key)
            return ScrubCommandDecision(
                ScrubCorrelationAction.IGNORE_ANOMALOUS,
                marker,
                record.key,
            )

        if marker.form == PublicCommandForm.DISPATCH:
            self._record_anomaly("acceptance_form_preceded_dispatch_form", marker, record.key)
        record.forms[marker.form] = marker
        return ScrubCommandDecision(
            ScrubCorrelationAction.ENRICH_EXISTING,
            marker,
            record.key,
        )

    def _observe_legacy(
        self, marker: ScrubCommandMarker
    ) -> ScrubCommandDecision:
        """Разрешает только источник с одной ID-less marker family."""

        self._legacy_seen = True
        if self._modern_seen:
            self._record_schema_mix(marker)
        if self._legacy_primary_form is None:
            self._legacy_primary_form = marker.form
        self._legacy_forms.add(marker.form)
        if marker.form != self._legacy_primary_form:
            if not self._legacy_mix_reported:
                self._record_anomaly("legacy_mixed_forms_without_id", marker, None)
                self._legacy_mix_reported = True
            return ScrubCommandDecision(
                ScrubCorrelationAction.IGNORE_ANOMALOUS,
                marker,
                None,
            )

        key = f"legacy:{self._next_legacy_sequence}"
        self._next_legacy_sequence += 1
        self._legacy_keys.append(key)
        return ScrubCommandDecision(
            ScrubCorrelationAction.APPLY_LOGICAL_COMMAND,
            marker,
            key,
        )

    def _record_schema_mix(self, marker: ScrubCommandMarker) -> None:
        """Публикует source-wide modern/legacy schema ambiguity ровно один раз."""

        if self._schema_mix_reported:
            return
        self._record_anomaly("mixed_scrub_identity_schema", marker, None)
        self._schema_mix_reported = True

    def _record_anomaly(
        self,
        kind: str,
        marker: ScrubCommandMarker,
        correlation_key: str | None,
    ) -> None:
        """Добавляет typed anomaly с bounded semantic fields."""

        target = marker.target
        self.anomalies.append(
            ScrubCommandAnomaly(
                sequence=self.first_anomaly_sequence + len(self.anomalies),
                source=self.source,
                kind=kind,
                line_number=marker.point.line_number,
                command_id=marker.command_id,
                form=marker.form.value,
                stage=marker.stage.value if marker.stage is not None else None,
                target_kind=target.kind if target is not None else None,
                target_ms=target.milliseconds if target is not None else None,
                correlation_key=correlation_key,
            )
        )


def parse_scrub_command_marker(
    line: str,
    point: LogPoint,
) -> ScrubCommandMarker | None:
    """Читает modern structured marker либо старую message-based form."""

    modern_schema = (
        field_value(line, "scrub_schema_version") is not None
        or field_value(line, "scrub_command_id") is not None
        or MODERN_SCRUB_MARKER in line
    )
    stage = _explicit_stage(line) if modern_schema else _legacy_stage(line)
    if stage is None and not modern_schema:
        return None
    form = _command_form(line, modern_schema)
    if modern_schema:
        return _parse_modern_marker(line, point, stage, form)
    return ScrubCommandMarker(
        point=point,
        form=form,
        stage=stage,
        command_id=None,
        target=_legacy_target_identity(stage, line),
        modern_schema=False,
    )


def line_is_scrub_command_marker(line: str) -> bool:
    """Fast-path predicate до полного structured parsing."""

    if MODERN_SCRUB_MARKER in line or "scrub_schema_version=" in line:
        return True
    if LEGACY_PUBLIC_COMMAND_MARKER not in line:
        return False
    return _legacy_stage(line) is not None


def _parse_modern_marker(
    line: str,
    point: LogPoint,
    stage: ScrubCommandStage | None,
    form: PublicCommandForm,
) -> ScrubCommandMarker:
    """Валидирует обязательные fields current Rust schema v1."""

    schema_version_field = _strict_unsigned_decimal_field(
        line,
        "scrub_schema_version",
        U64_MAX,
    )
    command_id_field = _strict_unsigned_decimal_field(
        line,
        "scrub_command_id",
        U64_MAX,
    )
    explicit_form = field_value(line, "scrub_command_form")
    target_kind = field_value(line, "scrub_target_kind")
    target_ms_field = _strict_unsigned_decimal_field(
        line,
        "scrub_requested_target_ms",
        U128_MAX,
    )
    schema_version = schema_version_field.value
    command_id = command_id_field.value
    target_ms = target_ms_field.value
    parse_failure: str | None = None
    if schema_version_field.failure == UnsignedDecimalFailure.MISSING:
        parse_failure = "missing_scrub_schema_version"
    elif schema_version_field.failure == UnsignedDecimalFailure.INVALID:
        parse_failure = "invalid_scrub_schema_version_integer"
    elif schema_version_field.failure == UnsignedDecimalFailure.OVERFLOW:
        parse_failure = "scrub_schema_version_overflow"
    elif schema_version != SCRUB_COMMAND_SCHEMA_VERSION:
        parse_failure = "unsupported_scrub_schema_version"
    elif explicit_form is None:
        parse_failure = "missing_scrub_command_form"
    elif explicit_form not in MODERN_FORM_ALIASES:
        parse_failure = "unsupported_scrub_command_form"
    elif (
        MODERN_FORM_ALIASES[explicit_form] == PublicCommandForm.ACCEPTANCE
        and field_value(line, "kind") != "seek_acceptance"
    ):
        parse_failure = "missing_seek_acceptance_kind"
    elif command_id_field.failure == UnsignedDecimalFailure.MISSING:
        parse_failure = "missing_scrub_command_id"
    elif command_id_field.failure == UnsignedDecimalFailure.INVALID:
        parse_failure = "invalid_scrub_command_id_integer"
    elif command_id_field.failure == UnsignedDecimalFailure.OVERFLOW:
        parse_failure = "scrub_command_id_overflow"
    elif command_id == 0:
        parse_failure = "invalid_scrub_command_id"
    elif stage is None:
        parse_failure = "missing_scrub_stage"
    elif target_ms_field.failure == UnsignedDecimalFailure.INVALID:
        parse_failure = "invalid_scrub_target_integer"
    elif target_ms_field.failure == UnsignedDecimalFailure.OVERFLOW:
        parse_failure = "scrub_target_overflow"
    elif (
        target_ms_field.failure == UnsignedDecimalFailure.MISSING
        or target_kind not in {"none", "absolute", "relative"}
    ):
        parse_failure = "missing_scrub_target_identity"
    elif stage in {ScrubCommandStage.BEGIN, ScrubCommandStage.END} and (
        target_kind != "none" or target_ms != 0
    ):
        parse_failure = "invalid_target_for_targetless_scrub_stage"
    elif stage in {ScrubCommandStage.UPDATE, ScrubCommandStage.PREVIEW} and (
        target_kind not in {"absolute", "relative"}
    ):
        parse_failure = "invalid_target_for_targeted_scrub_stage"

    target = (
        ScrubTargetIdentity(target_kind, target_ms)
        if target_kind is not None and target_ms is not None
        else None
    )
    return ScrubCommandMarker(
        point=point,
        form=form,
        stage=stage,
        command_id=command_id,
        target=target,
        modern_schema=True,
        parse_failure=parse_failure,
    )


def _command_form(line: str, modern_schema: bool) -> PublicCommandForm:
    """Modern schema требует explicit form; legacy сохраняет kind fallback."""

    explicit_form = field_value(line, "scrub_command_form")
    if explicit_form in MODERN_FORM_ALIASES:
        return MODERN_FORM_ALIASES[explicit_form]
    if field_value(line, "kind") == "seek_acceptance":
        return PublicCommandForm.ACCEPTANCE
    if modern_schema and explicit_form is not None:
        return PublicCommandForm.DISPATCH
    return PublicCommandForm.DISPATCH


def _strict_unsigned_decimal_field(
    line: str,
    field_name: str,
    maximum: int,
) -> UnsignedDecimalField:
    """Читает только `[0-9]+` без float rounding, exponent или non-finite values."""

    raw_value = field_value(line, field_name)
    if raw_value is None:
        return UnsignedDecimalField(None, UnsignedDecimalFailure.MISSING)
    if re.fullmatch(r"[0-9]+", raw_value) is None:
        return UnsignedDecimalField(None, UnsignedDecimalFailure.INVALID)
    normalized_value = raw_value.lstrip("0") or "0"
    maximum_decimal = str(maximum)
    if len(normalized_value) > len(maximum_decimal) or (
        len(normalized_value) == len(maximum_decimal)
        and normalized_value > maximum_decimal
    ):
        return UnsignedDecimalField(None, UnsignedDecimalFailure.OVERFLOW)
    exact_value = int(normalized_value, 10)
    return UnsignedDecimalField(exact_value, None)


def _explicit_stage(line: str) -> ScrubCommandStage | None:
    """Читает current structured stage без анализа full Debug command."""

    value = field_value(line, "scrub_stage")
    try:
        return ScrubCommandStage(value) if value is not None else None
    except ValueError:
        return None


def _legacy_stage(line: str) -> ScrubCommandStage | None:
    """Поддерживает historical single-form logs без synthetic pairing."""

    for command, stage in (
        ("command=BeginScrub", ScrubCommandStage.BEGIN),
        ("command=UpdateScrub", ScrubCommandStage.UPDATE),
        ("command=PreviewScrub", ScrubCommandStage.PREVIEW),
        ("command=EndScrub", ScrubCommandStage.END),
    ):
        if command in line:
            return stage
    return None


def _legacy_target_identity(
    stage: ScrubCommandStage,
    line: str,
) -> ScrubTargetIdentity:
    """Legacy target остаётся diagnostic identity, а не correlation heuristic."""

    if stage in {ScrubCommandStage.BEGIN, ScrubCommandStage.END}:
        return ScrubTargetIdentity("none", 0)
    explicit_target = float_field(line, "target_ms", "target_milliseconds")
    if explicit_target is not None:
        return ScrubTargetIdentity("absolute", int(explicit_target))
    match = LEGACY_TARGET_PATTERN.search(line)
    if match is None:
        return ScrubTargetIdentity("unknown", -1)
    value = float(match.group("value"))
    milliseconds = value if match.group("unit") == "ms" else value * 1000.0
    return ScrubTargetIdentity(match.group("kind").lower(), int(milliseconds))
