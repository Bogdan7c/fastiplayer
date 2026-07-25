#!/usr/bin/env python3
"""Проверяет focused S42 ownership, parser, FFmpeg и module-size guardrails."""

# Future annotations упрощают type hints без runtime forward-reference boilerplate.
from __future__ import annotations

# json читает Cargo metadata и checked-in module-size snapshot.
import json
# re задаёт узкие declaration patterns вместо поиска случайных слов.
import re
# subprocess запускает единственный locked Cargo metadata boundary.
import subprocess
# sys публикует actionable diagnostics и process status.
import sys
# dataclass хранит одно неизменяемое нарушение.
from dataclasses import dataclass
# pathlib удерживает все path checks платформенно-нейтральными.
from pathlib import Path
# Any описывает проверяемый внешний JSON без ложной type certainty.
from typing import Any

# Отдельный owner удерживает module-size schema/snapshot logic вне этого audit module.
from s42_module_size_guardrail import (
    ModuleSizeInputError,
    ModuleSizeViolation,
    find_module_size_violations,
    read_module_size_baseline,
)
# Отдельный owner удерживает exact F4F exception вне orchestration module.
from s42_f4f_guardrail import (
    F4fParserViolation,
    find_f4f_fmp4_source_violations,
    find_required_adapter_path_violations,
)


# Metadata всегда строится exact primary Rust и не может изменить Cargo.lock.
METADATA_COMMAND = (
    "cargo",
    "+1.96.0",
    "metadata",
    "--locked",
    "--no-deps",
    "--format-version",
    "1",
)

# HTTP client implementation принадлежит только source-core transport owner-у.
HTTP_CLIENT_DEPENDENCIES = frozenset(
    {"attohttpc", "curl", "hyper", "isahc", "reqwest", "surf", "ureq"}
)

# Альтернативные TS/FLV/ISO-BMFF crates создали бы второй container parser.
DUPLICATE_CONTAINER_PARSER_DEPENDENCIES = frozenset(
    {
        "flv",
        "flv-rs",
        "flvparse",
        "fmp4",
        "isobmff",
        "mp4",
        "mp4-atom",
        "mp4parse",
        "mp4parse-capi",
        "mpeg-ts",
        "mpeg2ts-reader",
        "mpegts",
        "tsparser",
    }
)

# Required normal edges закрепляют единственных HTTP/cache/prefetch owners.
REQUIRED_NORMAL_DEPENDENCIES = {
    "source-core": frozenset({"reqwest"}),
    "media-prefetch": frozenset({"source-core"}),
    "web-media-http": frozenset({"media-prefetch", "source-core"}),
    "service-direct-media": frozenset({"media-prefetch", "web-media-http"}),
}

# Test modules не являются production module-size/parser implementation surface.
INLINE_TEST_MODULE_START = re.compile(
    r"(?m)^\s*#\[cfg\(test\)\]\s*\n\s*mod\s+tests\s*\{"
)

# Сильные MPEG-TS declarations принадлежат только mpeg-ts-demux.
MPEG_TS_PATTERNS = (
    re.compile(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+(?:parse|read)_"
        r"(?:pat|pmt|pes|transport_packet|adaptation_field)\b",
        re.IGNORECASE,
    ),
    re.compile(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const|static)\s+TS_PACKET_SIZE\b",
        re.IGNORECASE,
    ),
)

# Сильные FLV header/tag declarations принадлежат только flv-demux.
FLV_PATTERNS = (
    re.compile(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?fn\s+(?:parse|read)_"
        r"(?:flv_header|flv_tag|script_tag|audio_tag|video_tag)\b",
        re.IGNORECASE,
    ),
)

# Encoder и output API запрещены даже внутри video-ffmpeg decode adapter-а.
FFMPEG_ENCODER_PATTERNS = (
    re.compile(
        r"\bavcodec_(?:find_encoder(?:_by_name)?|send_frame|receive_packet|"
        r"encode_audio2|encode_video2)\b"
    ),
    re.compile(
        r"\b(?:av_(?:interleaved_)?write_frame|avformat_(?:alloc_output_context2|"
        r"write_header)|AV_CODEC_FLAG_GLOBAL_HEADER)\b"
    ),
)

# HTTP byte-range cache implementation declarations остаются внутри source-core.
HTTP_CACHE_PATTERNS = (
    re.compile(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+\w*"
        r"(?:HttpCache|ByteRangeCache|CachedByteSource)\w*\b"
    ),
)

# Exact byte-prefetch implementation declarations остаются внутри media-prefetch.
PREFETCH_PATTERNS = (
    re.compile(
        r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+"
        r"(?:PrefetchWorker|PrefetchBufferState|PrefetchingByteSource|"
        r"PrefetchShared(?:State)?)\b"
    ),
)

# Legacy service-owned WebM opener symbols не должны вернуться в code/tooling.
LEGACY_WEBM_PATTERNS = (
    re.compile(
        r"\b(?:open_streaming_media_from|open_seekable_vod_from|"
        r"YtDlpStreamingMedia|YtDlpSelectedStreamIdentity|"
        r"selected_webm_(?:opens|falls_back|live))"
    ),
)

# Runtime scripts входят в WebM legacy audit вместе с Rust production source.
LEGACY_WEBM_SCRIPT_PATHS = (
    Path("scripts/media-regression.sh"),
    Path("scripts/playback-smoke.sh"),
    Path("scripts/progressive-web-smoke.sh"),
    Path("scripts/runtime-acceptance.sh"),
)


# Одна запись нарушения сохраняет owner/path и понятное правило.
@dataclass(frozen=True)
class Violation:
    """Одно S42 guardrail нарушение."""

    # Location называет package, manifest owner или relative source path.
    location: str
    # Rule объясняет нарушенный архитектурный инвариант.
    rule: str
    # Evidence показывает exact dependency, symbol или line-count delta.
    evidence: str


# Ошибка входных данных отличается от обычного architecture violation.
class GuardrailInputError(RuntimeError):
    """S42 guardrail не смог достоверно прочитать repository inputs."""


# Все focused modules публикуют одинаковый diagnostic shape.
GuardrailViolation = Violation | F4fParserViolation | ModuleSizeViolation


# Функция вычисляет repository root относительно versioned script-а.
def repository_root() -> Path:
    """Возвращает корень репозитория."""

    # parents[1] соответствует scripts/.. без зависимости от cwd.
    return Path(__file__).resolve().parents[1]


# Функция запускает locked metadata и сохраняет полную Cargo diagnostics.
def load_workspace_packages(repo_root: Path) -> dict[str, dict[str, Any]]:
    """Возвращает workspace packages по Cargo package name."""

    # Capture позволяет превратить Cargo failure в одну понятную input error.
    completed = subprocess.run(
        METADATA_COMMAND,
        cwd=repo_root,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    # Ненулевой status запрещает продолжать по stale/partial graph.
    if completed.returncode != 0:
        # stderr важнее пустого stdout для Cargo resolution failures.
        diagnostics = completed.stderr.strip() or completed.stdout.strip()
        # Exact command и diagnostics ускоряют local remediation.
        raise GuardrailInputError(
            f"`{' '.join(METADATA_COMMAND)}` failed ({completed.returncode}): {diagnostics}"
        )
    # JSON decode errors считаются input failure, а не отсутствием violations.
    try:
        # Parsed object нужен только внутри проверяемого metadata schema.
        metadata = json.loads(completed.stdout)
    except json.JSONDecodeError as error:
        # Причина сохраняет line/column invalid JSON.
        raise GuardrailInputError(f"cargo metadata вернул невалидный JSON: {error}") from error
    # Package rows индексируются сначала по opaque Cargo id.
    packages_by_id = {
        package["id"]: package
        for package in metadata.get("packages", [])
        if isinstance(package, dict) and isinstance(package.get("id"), str)
    }
    # Workspace membership исключает standalone upstream patch crates.
    workspace_packages: dict[str, dict[str, Any]] = {}
    # Каждый member обязан разрешаться в ровно один package.
    for package_id in metadata.get("workspace_members", []):
        # Missing package row означает broken metadata contract.
        package = packages_by_id.get(package_id)
        # Невалидный id нельзя молча пропустить.
        if package is None:
            raise GuardrailInputError(f"workspace member `{package_id}` отсутствует в packages")
        # Package name является стабильной owner identity для diagnostics.
        package_name = package.get("name")
        # Нестроковое или duplicate имя разрушает dependency map.
        if not isinstance(package_name, str) or package_name in workspace_packages:
            raise GuardrailInputError(f"невалидное/duplicate package name `{package_name}`")
        # Validated row сохраняется без неявной нормализации.
        workspace_packages[package_name] = package
    # Пустой workspace не может доказать release architecture.
    if not workspace_packages:
        raise GuardrailInputError("cargo metadata не содержит workspace packages")
    # Полный map используется dependency и source inventory проверками.
    return workspace_packages


# Функция строит normal/all direct dependency maps из одного metadata snapshot.
def dependency_maps(
    packages: dict[str, dict[str, Any]],
) -> tuple[dict[str, frozenset[str]], dict[str, frozenset[str]]]:
    """Возвращает normal и all-kind direct dependency maps."""

    # Normal map отвечает production dependency ownership.
    normal_dependencies: dict[str, frozenset[str]] = {}
    # All-kind map не позволяет спрятать второй HTTP stack в build/dev edge.
    all_dependencies: dict[str, frozenset[str]] = {}
    # Каждый workspace owner проверяется независимо.
    for package_name, package in packages.items():
        # Cargo metadata обязан отдавать dependency array.
        dependencies = package.get("dependencies")
        # Broken row делает audit недостоверным.
        if not isinstance(dependencies, list):
            raise GuardrailInputError(f"package `{package_name}` не содержит dependencies")
        # Dependency name берётся из Cargo resolved package vocabulary.
        all_names = {
            dependency["name"]
            for dependency in dependencies
            if isinstance(dependency, dict) and isinstance(dependency.get("name"), str)
        }
        # Normal dependency имеет Cargo metadata kind null.
        normal_names = {
            dependency["name"]
            for dependency in dependencies
            if isinstance(dependency, dict)
            and isinstance(dependency.get("name"), str)
            and dependency.get("kind") is None
        }
        # Immutable sets не могут быть случайно изменены следующей policy.
        normal_dependencies[package_name] = frozenset(normal_names)
        # All-kind map используется только isolation rules.
        all_dependencies[package_name] = frozenset(all_names)
    # Обе карты возвращаются из одной функции, исключая разные snapshots.
    return normal_dependencies, all_dependencies


# Функция проверяет single HTTP/prefetch и container parser dependency ownership.
def find_dependency_violations(
    normal_dependencies: dict[str, frozenset[str]],
    all_dependencies: dict[str, frozenset[str]],
) -> list[Violation]:
    """Возвращает S42 direct dependency нарушения."""

    # Список агрегирует все owners за один запуск.
    violations: list[Violation] = []
    # Только source-core может напрямую подключать HTTP client crate.
    for owner, dependencies in sorted(all_dependencies.items()):
        # Source owner является единственным разрешённым исключением.
        if owner == "source-core":
            continue
        # Каждая найденная библиотека получает отдельную diagnostics.
        for dependency in sorted(dependencies & HTTP_CLIENT_DEPENDENCIES):
            violations.append(
                Violation(
                    location=owner,
                    rule="HTTP client dependency разрешена только source-core",
                    evidence=dependency,
                )
            )
    # Второй TS/FLV/fMP4 parser запрещён в любом production package.
    for owner, dependencies in sorted(normal_dependencies.items()):
        # Пересечение оставляет только известные alternative parser crates.
        for dependency in sorted(dependencies & DUPLICATE_CONTAINER_PARSER_DEPENDENCIES):
            violations.append(
                Violation(
                    location=owner,
                    rule="alternative TS/FLV/fMP4 parser dependency запрещена",
                    evidence=dependency,
                )
            )
    # Required edges доказывают reuse текущих HTTP/cache/prefetch owners.
    for owner, required_dependencies in REQUIRED_NORMAL_DEPENDENCIES.items():
        # Missing owner означает неполный release workspace.
        actual_dependencies = normal_dependencies.get(owner)
        # Отсутствующий owner и missing edges перечисляются одним правилом.
        missing_dependencies = (
            required_dependencies
            if actual_dependencies is None
            else required_dependencies - actual_dependencies
        )
        # Каждая missing edge остаётся точечной.
        for dependency in sorted(missing_dependencies):
            violations.append(
                Violation(
                    location=owner,
                    rule="обязательный HTTP/cache/prefetch ownership edge отсутствует",
                    evidence=dependency,
                )
            )
    # Stable ordering делает CI output воспроизводимым.
    return sorted(violations, key=lambda item: (item.location, item.rule, item.evidence))


# Функция возвращает production Rust files каждого workspace member-а.
def production_rust_files(
    repo_root: Path,
    packages: dict[str, dict[str, Any]],
) -> list[Path]:
    """Возвращает relative production Rust paths без test modules/files."""

    # Set устраняет duplicate path при неожиданном shared target metadata.
    source_files: set[Path] = set()
    # Manifest directory задаёт точный crate root.
    for package in packages.values():
        # Cargo отдаёт абсолютный manifest_path.
        manifest_path = package.get("manifest_path")
        # Broken path не должен уменьшить audited source inventory.
        if not isinstance(manifest_path, str):
            raise GuardrailInputError("workspace package не содержит manifest_path")
        # Только src/ считается production module tree.
        source_root = Path(manifest_path).parent / "src"
        # Crate без src directory требует отдельного manifest decision.
        if not source_root.is_dir():
            raise GuardrailInputError(f"workspace source root отсутствует: {source_root}")
        # Rust modules собираются рекурсивно.
        for source_path in source_root.rglob("*.rs"):
            # Dedicated test files/directories не являются production module debt.
            if (
                "tests" in source_path.parts
                or source_path.name == "tests.rs"
                or source_path.name.endswith("_tests.rs")
            ):
                continue
            # Relative path стабилен локально и в CI.
            source_files.add(source_path.relative_to(repo_root))
    # Сортировка стабилизирует baseline и diagnostics.
    return sorted(source_files)


# Функция удаляет inline cfg(test) tail из declaration audit-а.
def production_source_text(source_text: str) -> str:
    """Возвращает production часть Rust source до inline tests."""

    # Первый canonical cfg(test) module отделяет production и fixture declarations.
    test_module_match = INLINE_TEST_MODULE_START.search(source_text)
    # Файл без inline tests используется целиком.
    if test_module_match is None:
        return source_text
    # Test tail не может создать production parser/API нарушение.
    return source_text[: test_module_match.start()]


# Функция сканирует parser/FFmpeg/cache/prefetch declarations.
def find_source_violations(
    repo_root: Path,
    source_files: list[Path],
) -> list[GuardrailViolation]:
    """Возвращает S42 production source нарушения."""

    # Все rules агрегируются без fail-fast.
    violations: list[GuardrailViolation] = []
    # Отдельный owner fail-closed проверяет наличие exact exception path.
    violations.extend(find_required_adapter_path_violations(source_files))
    # Каждый production module читается ровно один раз.
    for relative_path in source_files:
        # UTF-8 является обязательным repository invariant.
        full_text = (repo_root / relative_path).read_text(encoding="utf-8")
        # Parser/API scan не видит inline tests.
        source_text = production_source_text(full_text)
        # Path parts позволяют проверять owner root без substring ambiguity.
        path_parts = relative_path.parts
        # MPEG-TS declarations разрешены только exact first-party owner-у.
        if path_parts[:2] != ("crates", "mpeg-ts-demux"):
            violations.extend(
                match_line_patterns(
                    relative_path,
                    source_text,
                    MPEG_TS_PATTERNS,
                    "MPEG-TS parsing принадлежит mpeg-ts-demux",
                )
            )
        # FLV declarations разрешены только exact first-party owner-у.
        if path_parts[:2] != ("crates", "flv-demux"):
            violations.extend(
                match_line_patterns(
                    relative_path,
                    source_text,
                    FLV_PATTERNS,
                    "FLV/F4F parsing принадлежит flv-demux",
                )
            )
        # Focused owner применяет exact F4F exception и generic fMP4 policy.
        violations.extend(
            find_f4f_fmp4_source_violations(relative_path, source_text)
        )
        # Decode-only rule применяется ко всему workspace, включая video-ffmpeg.
        violations.extend(
            match_line_patterns(
                relative_path,
                source_text,
                FFMPEG_ENCODER_PATTERNS,
                "FFmpeg encoder/output API запрещён decode-only boundary",
            )
        )
        # HTTP byte cache declaration разрешена только source-core.
        if path_parts[:2] != ("crates", "source-core"):
            violations.extend(
                match_line_patterns(
                    relative_path,
                    source_text,
                    HTTP_CACHE_PATTERNS,
                    "HTTP byte cache implementation принадлежит source-core",
                )
            )
        # Byte prefetch declaration разрешена только media-prefetch.
        if path_parts[:2] != ("crates", "media-prefetch"):
            violations.extend(
                match_line_patterns(
                    relative_path,
                    source_text,
                    PREFETCH_PATTERNS,
                    "byte prefetch implementation принадлежит media-prefetch",
                )
            )
    # Stable ordering делает review deterministic.
    return sorted(violations, key=lambda item: (item.location, item.rule, item.evidence))


# Функция превращает regex matches одного файла в line-addressable violations.
def match_line_patterns(
    relative_path: Path,
    source_text: str,
    patterns: tuple[re.Pattern[str], ...],
    rule: str,
) -> list[Violation]:
    """Ищет patterns построчно в одном production source."""

    # Локальный список сохраняет все matches.
    violations: list[Violation] = []
    # Нумерация начинается с единицы для editor/CI diagnostics.
    for line_number, line in enumerate(source_text.splitlines(), start=1):
        # Один line может нарушать только одно и то же агрегированное rule.
        if not any(pattern.search(line) for pattern in patterns):
            continue
        # Exact line сохраняется bounded и без raw runtime secrets.
        violations.append(
            Violation(
                location=f"{relative_path}:{line_number}",
                rule=rule,
                evidence=line.strip(),
            )
        )
    # Caller объединяет результаты разных rules.
    return violations


# Функция отдельно проверяет удалённый legacy WebM opener в source и tooling.
def find_legacy_webm_violations(
    repo_root: Path,
    source_files: list[Path],
) -> list[Violation]:
    """Возвращает legacy service-owned WebM opener matches."""

    # App/service production source образуют runtime legacy surface.
    audited_paths = [
        relative_path
        for relative_path in source_files
        if relative_path.parts[:2] in {
            ("crates", "app-egui"),
            ("crates", "service-ytdlp"),
        }
    ]
    # Checked runtime scripts дополняют Rust source.
    audited_paths.extend(LEGACY_WEBM_SCRIPT_PATHS)
    # Сканирование использует тот же line-addressable helper.
    violations: list[Violation] = []
    # Missing checked script является input failure, не молчаливым pass.
    for relative_path in sorted(set(audited_paths)):
        # Полный path проверяется до чтения.
        source_path = repo_root / relative_path
        # Runtime evidence path обязан существовать.
        if not source_path.is_file():
            raise GuardrailInputError(f"legacy WebM audit path отсутствует: {relative_path}")
        # Rust inline tests не должны влиять на production legacy status.
        source_text = source_path.read_text(encoding="utf-8")
        # Shell scripts не содержат Rust cfg(test) module.
        if source_path.suffix == ".rs":
            source_text = production_source_text(source_text)
        # Exact old symbols превращаются в нарушения.
        violations.extend(
            match_line_patterns(
                relative_path,
                source_text,
                LEGACY_WEBM_PATTERNS,
                "legacy service-owned WebM opener запрещён",
            )
        )
    # Stable order сохраняет deterministic output.
    return sorted(violations, key=lambda item: (item.location, item.evidence))


# Функция печатает все violations в одном bounded CI block.
def print_violations(violations: list[GuardrailViolation]) -> None:
    """Печатает actionable S42 failure report."""

    # Общий заголовок отделяет policy failure от Cargo/test failure.
    print("S42 guardrails: FAILED", file=sys.stderr)
    # Каждая строка содержит owner/path, rule и exact evidence.
    for violation in violations:
        # Colon-separated формат остаётся удобным для terminal/editor.
        print(
            f"  - {violation.location}: {violation.rule}: {violation.evidence}",
            file=sys.stderr,
        )


# Функция связывает один metadata snapshot и все focused policies.
def run() -> int:
    """Запускает S42 guardrails и возвращает process status."""

    # Root вычисляется один раз.
    repo_root = repository_root()
    # Locked metadata является единственным dependency/workspace input.
    packages = load_workspace_packages(repo_root)
    # Dependency maps строятся из того же snapshot.
    normal_dependencies, all_dependencies = dependency_maps(packages)
    # Production source inventory также берётся из exact workspace members.
    source_files = production_rust_files(repo_root, packages)
    # Все независимые policies публикуют полный результат.
    violations = find_dependency_violations(normal_dependencies, all_dependencies)
    # Parser/FFmpeg/cache/prefetch declarations проверяются отдельно.
    violations.extend(find_source_violations(repo_root, source_files))
    # Legacy WebM audit включает production source и checked runtime scripts.
    violations.extend(find_legacy_webm_violations(repo_root, source_files))
    # Module-size snapshot читается только после source inventory.
    baseline = read_module_size_baseline(repo_root)
    # Exact snapshot запрещает новый debt и скрытый рост legacy modules.
    violations.extend(find_module_size_violations(repo_root, source_files, baseline))
    # Любое нарушение блокирует S42 gate.
    if violations:
        # Полный report печатается до ненулевого status.
        print_violations(sorted(violations, key=lambda item: (item.location, item.rule)))
        # Код 1 обозначает architecture policy violation.
        return 1
    # Явный success виден в локальном и CI log.
    print("S42 guardrails: OK")
    # Нулевой status подтверждает все focused invariants.
    return 0


# Функция отделяет input/tool failure от найденного violation.
def main() -> None:
    """Запускает guardrail с понятной error diagnostics."""

    # Expected input errors не должны печатать Python traceback.
    try:
        # Pure integer status передаётся system boundary.
        exit_code = run()
    except (GuardrailInputError, ModuleSizeInputError, OSError, UnicodeError) as error:
        # Код 2 сообщает broken audit input/tooling.
        print(f"S42 guardrails: ERROR: {error}", file=sys.stderr)
        # Отдельный status помогает отличить architecture regression.
        exit_code = 2
    # SystemExit сохраняет status для shell/CI owner-а.
    raise SystemExit(exit_code)


# Import unit-тестами не запускает repository scan.
if __name__ == "__main__":
    # CLI использует единственную main boundary.
    main()
