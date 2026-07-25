#!/usr/bin/env python3
"""Владеет exact S42 F4F exception и generic fMP4 parser guardrail."""

# Future annotations сохраняют type hints без runtime forward references.
from __future__ import annotations

# Counter сохраняет duplicate declarations, которые потерял бы обычный set.
from collections import Counter
# Collection принимает immutable или mutable source-file inventory.
from collections.abc import Collection
# dataclass хранит одно неизменяемое parser violation.
from dataclasses import dataclass
# pathlib удерживает repository identities платформенно-нейтральными.
from pathlib import Path
# re задаёт узкие Rust declaration patterns.
import re


# Единственное owner-approved исключение — узкий F4F ISO-envelope adapter.
F4F_ISO_ENVELOPE_ADAPTER_PATH = Path("crates/flv-demux/src/f4f.rs")

# Declaration names сгруппированы по Rust kind для читаемого exact ratchet-а.
F4F_ISO_ENVELOPE_SYMBOLS_BY_KIND = {
    "const": frozenset(
        "BOX_HEADER_BYTES FULL_BOX_HEADER_BYTES HEADER_FLAGS "
        "LARGE_BOX_HEADER_BYTES OPTIONAL_FIELD_FLAGS SAMPLE_FLAGS SEMANTIC_FLAGS".split()
    ),
    "fn": frozenset(
        "consume_box_budget finish malformed new parse_box_at parse_boxes "
        "parse_f4f_segment read read_bounded_count read_box read_full_box "
        "read_string read_strings read_u32 read_u8 skip skip_repeated "
        "validate_abst validate_afra validate_afrt validate_asrt "
        "validate_fixed_full_box validate_moof validate_table_count "
        "validate_tfhd validate_traf validate_trun".split()
    ),
    "struct": frozenset("IsoBox ParsedF4fSegment PayloadCursor".split()),
}

# Flattened kind/name pairs сравниваются с regex inventory без неявных aliases.
F4F_ISO_ENVELOPE_DECLARATION_SYMBOLS = frozenset(
    (symbol_kind, symbol_name)
    for symbol_kind, symbol_names in F4F_ISO_ENVELOPE_SYMBOLS_BY_KIND.items()
    for symbol_name in symbol_names
)

# Общий prefix не позволяет qualifiers спрятать function declaration.
RUST_FUNCTION_DECLARATION_PREFIX = (
    r"^\s*(?:pub(?:\([^)]*\))?\s+)?"
    r"(?:(?:const|async|unsafe)\s+|extern(?:\s+\"[^\"]+\")?\s+)*fn\s+"
)

# Function inventory использует тот же prefix, что rogue parser patterns.
RUST_FUNCTION_DECLARATION_PATTERN = re.compile(
    RUST_FUNCTION_DECLARATION_PREFIX + r"([A-Za-z_][A-Za-z0-9_]*)\b",
    re.MULTILINE,
)

# Types и constants не имеют function qualifiers и проверяются отдельно.
RUST_NON_FUNCTION_DECLARATION_PATTERN = re.compile(
    r"(?m)^\s*(?:pub(?:\([^)]*\))?\s+)?"
    r"(const|static|struct|enum|union|trait|type)\s+"
    r"(?!(?:const|async|unsafe|extern|fn)\b)"
    r"([A-Za-z_][A-Za-z0-9_]*)\b"
)

# Generic fMP4 atom parse declarations принадлежат exact ISO-BMFF patch.
FMP4_PATTERNS = (
    re.compile(
        RUST_FUNCTION_DECLARATION_PREFIX + r"(?:parse|read)_"
        r"(?:ftyp|moov|moof|traf|tfhd|tfdt|trun|mdat|sidx|emsg)\b",
        re.IGNORECASE,
    ),
    re.compile(
        RUST_FUNCTION_DECLARATION_PREFIX + r"parse_(?:boxes|box_at)\b",
        re.IGNORECASE,
    ),
    re.compile(
        RUST_FUNCTION_DECLARATION_PREFIX + r"validate_"
        r"(?:moof|traf|tfhd|tfdt|trun|mdat)\b",
        re.IGNORECASE,
    ),
)

# F4F-specific declarations допустимы только в exact ratcheted adapter path.
F4F_ISO_ENVELOPE_PATTERNS = (
    re.compile(
        RUST_FUNCTION_DECLARATION_PREFIX
        + r"(?:parse_f4f_segment|validate_(?:afra|abst|asrt|afrt|fixed_full_box))\b",
        re.IGNORECASE,
    ),
)


# Одна parser diagnostics сохраняет path, policy и exact evidence.
@dataclass(frozen=True)
class F4fParserViolation:
    """Одно F4F/fMP4 guardrail нарушение."""

    # Location называет relative source path и optional line.
    location: str
    # Rule объясняет нарушенную owner boundary.
    rule: str
    # Evidence показывает exact missing/unexpected declaration или source line.
    evidence: str


# Функция требует присутствия exception только в exact repository path.
def find_required_adapter_path_violations(
    source_files: Collection[Path],
) -> list[F4fParserViolation]:
    """Возвращает violation при отсутствии exact F4F adapter path."""

    # Membership не зависит от ordering или duplicate inventory rows.
    if F4F_ISO_ENVELOPE_ADAPTER_PATH in source_files:
        # Exact path присутствует; symbol ratchet выполнится при чтении файла.
        return []
    # Missing path означает незаявленный переезд или удаление исключения.
    return [
        F4fParserViolation(
            location=str(F4F_ISO_ENVELOPE_ADAPTER_PATH),
            rule="exact F4F ISO-envelope adapter path отсутствует",
            evidence=str(F4F_ISO_ENVELOPE_ADAPTER_PATH),
        )
    ]


# Функция применяет exact exception либо общую no-duplicate parser policy.
def find_f4f_fmp4_source_violations(
    relative_path: Path,
    source_text: str,
) -> list[F4fParserViolation]:
    """Возвращает F4F/fMP4 violations одного production Rust source."""

    # Единственный exact path проходит полный declaration ratchet.
    if relative_path == F4F_ISO_ENVELOPE_ADAPTER_PATH:
        # Ratchet разрешает текущий bounded envelope, но не generic demux growth.
        return _find_adapter_symbol_violations(source_text)
    # Все остальные modules проверяются как обычная production surface.
    violations = _match_line_patterns(
        relative_path,
        source_text,
        F4F_ISO_ENVELOPE_PATTERNS,
        "F4F ISO-envelope adapter разрешён только в exact flv-demux path",
    )
    # Generic fMP4 parser declarations остаются у standalone ISO-BMFF patch.
    violations.extend(
        _match_line_patterns(
            relative_path,
            source_text,
            FMP4_PATTERNS,
            "generic fMP4 parsing принадлежит symphonia-format-isomp4 patch",
        )
    )
    # Stable rule/evidence order делает CI output воспроизводимым.
    return sorted(violations, key=lambda item: (item.location, item.rule, item.evidence))


# Функция ratchet-ит declarations единственного разрешённого adapter-а.
def _find_adapter_symbol_violations(source_text: str) -> list[F4fParserViolation]:
    """Возвращает изменения exact F4F ISO-envelope symbol inventory."""

    # Counter сначала сохраняет qualified и bare function declarations.
    actual_symbol_counts = Counter(
        ("fn", symbol_name)
        for symbol_name in RUST_FUNCTION_DECLARATION_PATTERN.findall(source_text)
    )
    # Types/constants дополняют тот же exact declaration inventory.
    actual_symbol_counts.update(
        RUST_NON_FUNCTION_DECLARATION_PATTERN.findall(source_text)
    )
    # Immutable keys нужны для двухстороннего inventory diff-а.
    actual_symbols = frozenset(actual_symbol_counts)
    # Missing, unexpected и duplicate изменения не сливаются в один boolean.
    inventory_changes = [
        *(
            ("missing", symbol)
            for symbol in F4F_ISO_ENVELOPE_DECLARATION_SYMBOLS - actual_symbols
        ),
        *(
            ("unexpected", symbol)
            for symbol in actual_symbols - F4F_ISO_ENVELOPE_DECLARATION_SYMBOLS
        ),
        *(
            ("duplicate", symbol)
            for symbol, count in actual_symbol_counts.items()
            if count > 1
        ),
    ]
    # Каждое изменение публикуется отдельной actionable diagnostics.
    violations: list[F4fParserViolation] = []
    # Сортировка стабилизирует simultaneous missing/unexpected output.
    for change_kind, (symbol_kind, symbol_name) in sorted(inventory_changes):
        # Function получает привычное diagnostic имя вместо Rust keyword.
        diagnostic_kind = "function" if symbol_kind == "fn" else symbol_kind
        # Duplicate дополнительно показывает exact multiplicity.
        count_suffix = (
            f" x{actual_symbol_counts[(symbol_kind, symbol_name)]}"
            if change_kind == "duplicate"
            else ""
        )
        # Exact path остаётся owner identity без raw source-content leakage.
        violations.append(
            F4fParserViolation(
                location=str(F4F_ISO_ENVELOPE_ADAPTER_PATH),
                rule="exact F4F ISO-envelope symbol inventory изменён",
                evidence=(
                    f"{change_kind} {diagnostic_kind} `{symbol_name}`{count_suffix}"
                ),
            )
        )
    # Loop уже стабилизировал полный result.
    return violations


# Функция превращает regex matches в line-addressable violations.
def _match_line_patterns(
    relative_path: Path,
    source_text: str,
    patterns: tuple[re.Pattern[str], ...],
    rule: str,
) -> list[F4fParserViolation]:
    """Ищет F4F/fMP4 declaration patterns построчно."""

    # Локальный список сохраняет все matches одного rule.
    violations: list[F4fParserViolation] = []
    # Нумерация начинается с единицы для editor/CI diagnostics.
    for line_number, line in enumerate(source_text.splitlines(), start=1):
        # Один line может нарушать только одно и то же агрегированное rule.
        if not any(pattern.search(line) for pattern in patterns):
            continue
        # Exact declaration line не содержит runtime locator/secrets.
        violations.append(
            F4fParserViolation(
                location=f"{relative_path}:{line_number}",
                rule=rule,
                evidence=line.strip(),
            )
        )
    # Caller объединяет F4F-specific и generic fMP4 results.
    return violations
