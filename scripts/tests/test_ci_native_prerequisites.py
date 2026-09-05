#!/usr/bin/env python3
"""Закрепляет S01/S03 CI-контракты toolchain, native deps и libva headers."""

from __future__ import annotations

import re
import shlex
import unittest
from pathlib import Path


# Корень репозитория вычисляется относительно этого focused test, а не cwd процесса.
REPOSITORY_ROOT = Path(__file__).resolve().parents[2]
# Основной workflow владеет format, Clippy и standalone dependency-patch gates.
CI_WORKFLOW_PATH = REPOSITORY_ROOT / ".github" / "workflows" / "ci.yml"
# Полное instrumented измерение вынесено в отдельный manual workflow.
COVERAGE_WORKFLOW_PATH = REPOSITORY_ROOT / ".github" / "workflows" / "coverage.yml"
# Отдельный workflow доказывает workspace check на primary toolchain и MSRV.
TOOLCHAIN_WORKFLOW_PATH = (
    REPOSITORY_ROOT / ".github" / "workflows" / "toolchain-policy.yml"
)
# Exact action revision уже принят проектом и не должен плавать между jobs.
RUST_TOOLCHAIN_ACTION = (
    "dtolnay/rust-toolchain@fa04a1451ff1842e2626ccb99004d0195b455a88"
)
# Эти packages следуют из workspace manifests и build scripts, а не из полного FFmpeg SDK.
EXPECTED_WORKSPACE_NATIVE_PACKAGES = frozenset(
    {
        "clang",
        "libclang-dev",
        "libasound2-dev",
        "libavcodec-dev",
        "libavutil-dev",
        "libdrm-dev",
        "libgbm-dev",
        "libva-dev",
        "pkg-config",
    }
)
# Tests и coverage компилируют Cargo examples и поэтому линкуют SoundTouch backend.
EXPECTED_ALL_TARGET_NATIVE_PACKAGES = frozenset(
    {
        "clang",
        "libclang-dev",
        "libasound2-dev",
        "libavcodec-dev",
        "libavutil-dev",
        "libgbm-dev",
        "libsoundtouch-dev",
        "libva-dev",
        "libvulkan1",
        "mesa-vulkan-drivers",
        "pkg-config",
    }
)
# Обе тяжёлые jobs обязаны отключать полный DWARF в Cargo test profile.
EXPECTED_ALL_TARGET_TEST_PROFILE_DEBUG = '      CARGO_PROFILE_TEST_DEBUG: "0"'
# Standalone cros-libva не компилирует audio/FFmpeg/GBM consumers.
EXPECTED_CROS_LIBVA_NATIVE_PACKAGES = frozenset(
    {"clang", "libclang-dev", "libva-dev", "pkg-config"}
)


def read_workflow(workflow_path: Path) -> str:
    """Читает workflow как UTF-8 для точного source-level контракта."""

    # GitHub workflow является versioned source и не требует YAML dependency в tests.
    return workflow_path.read_text(encoding="utf-8")


def extract_job(workflow_source: str, job_identifier: str) -> str:
    """Извлекает один top-level job по фиксированному двухпробельному отступу."""

    # Job id экранируется, чтобы дефисы не меняли смысл регулярного выражения.
    job_pattern = re.compile(
        rf"^  {re.escape(job_identifier)}:\n"
        rf"(?P<body>.*?)"
        rf"(?=^  [A-Za-z0-9_-]+:\n|\Z)",
        re.MULTILINE | re.DOTALL,
    )
    # Отсутствующий job должен дать понятный focused failure, а не AttributeError.
    job_match = job_pattern.search(workflow_source)
    # AssertionError показывает exact job id при структурном drift workflow.
    if job_match is None:
        raise AssertionError(f"workflow job `{job_identifier}` не найден")
    # Возвращаем job вместе с header, чтобы tests могли проверять его id при необходимости.
    return f"  {job_identifier}:\n{job_match.group('body')}"


def extract_named_step(job_source: str, step_name: str) -> str:
    """Извлекает именованный step внутри уже ограниченного job source."""

    # Шестипробельный marker отличает steps от nested mappings `with` и `env`.
    step_pattern = re.compile(
        rf"^      - name: {re.escape(step_name)}\n"
        rf"(?P<body>.*?)"
        rf"(?=^      - |\Z)",
        re.MULTILINE | re.DOTALL,
    )
    # Missing step означает потерю проверяемого CI boundary.
    step_match = step_pattern.search(job_source)
    # Ошибка называет exact step, который нужно восстановить.
    if step_match is None:
        raise AssertionError(f"workflow step `{step_name}` не найден")
    # Header включается, чтобы условие `if` и run body проверялись одним source block.
    return f"      - name: {step_name}\n{step_match.group('body')}"


def apt_install_packages(step_source: str) -> frozenset[str]:
    """Возвращает exact package set единственной apt-get install команды шага."""

    # Команда обязана оставаться однострочной: так source audit видит полный dependency set.
    install_lines = [
        line.strip()
        for line in step_source.splitlines()
        if line.strip().startswith("sudo apt-get install ")
    ]
    # Несколько install-команд позволили бы скрыть лишний или непроверяемый package list.
    if len(install_lines) != 1:
        raise AssertionError(
            "ожидалась ровно одна `sudo apt-get install` команда, "
            f"получено: {install_lines}"
        )
    # shlex учитывает shell quoting и не исполняет workflow source.
    install_arguments = shlex.split(install_lines[0])
    # Exact prefix закрепляет noninteractive минимальную установку без recommends.
    expected_prefix = [
        "sudo",
        "apt-get",
        "install",
        "--yes",
        "--no-install-recommends",
    ]
    # Drift флагов меняет воспроизводимость runner и должен быть видимым failure.
    if install_arguments[: len(expected_prefix)] != expected_prefix:
        raise AssertionError(
            f"неожиданный apt install prefix: {install_arguments[:len(expected_prefix)]}"
        )
    # Set сравнивает смысл package inventory независимо от безопасного порядка имён.
    return frozenset(install_arguments[len(expected_prefix) :])


class CiNativePrerequisitesTests(unittest.TestCase):
    """Проверяет реальные S01/S03 workflow owners и compatibility branches."""

    @classmethod
    def setUpClass(cls) -> None:
        """Читает production workflows один раз для всего focused suite."""

        # Основной CI source нужен четырём независимым contract tests.
        cls.ci_workflow = read_workflow(CI_WORKFLOW_PATH)
        cls.coverage_workflow = read_workflow(COVERAGE_WORKFLOW_PATH)
        # Toolchain source нужен exact native dependency inventory test.
        cls.toolchain_workflow = read_workflow(TOOLCHAIN_WORKFLOW_PATH)

    def test_quality_jobs_install_exact_components_explicitly(self) -> None:
        """Format и Clippy jobs не зависят от неполного runner tool cache."""

        # Format job владеет read-only rustfmt gate.
        format_job = extract_job(self.ci_workflow, "format-guardrails")
        # Pinned action устанавливает тот же exact primary compiler, что и repo policy.
        self.assertIn(f"- uses: {RUST_TOOLCHAIN_ACTION}", format_job)
        # Явный component гарантирует доступность cargo fmt на чистом runner.
        self.assertIn("toolchain: 1.96.0\n          components: rustfmt", format_job)

        # Strict Clippy job владеет all-features/all-targets lint gate.
        clippy_job = extract_job(self.ci_workflow, "clippy")
        # Тот же pinned action исключает floating installer behavior.
        self.assertIn(f"- uses: {RUST_TOOLCHAIN_ACTION}", clippy_job)
        # Явный component устраняет подтверждённый `clippy is not installed` failure.
        self.assertIn("toolchain: 1.96.0\n          components: clippy", clippy_job)

    def test_toolchain_workspace_check_installs_exact_native_inventory(self) -> None:
        """Workspace matrix получает все и только реально нужные native SDK."""

        # Оба matrix toolchains исполняют один и тот же workspace-check job.
        workspace_job = extract_job(self.toolchain_workflow, "workspace-check")
        # Именованный step является единым владельцем apt inventory этого job.
        install_step = extract_named_step(
            workspace_job,
            "Install workspace native prerequisites",
        )
        # Exact set ловит и missing ALSA/VA/GBM, и возврат ненужного полного FFmpeg SDK.
        self.assertEqual(
            EXPECTED_WORKSPACE_NATIVE_PACKAGES,
            apt_install_packages(install_step),
        )

    def test_all_target_jobs_have_bounded_artifacts_and_native_dependencies(self) -> None:
        """Tests и coverage VM получают bounded profile и все native libraries."""

        # Каждая GitHub-hosted job работает на отдельной VM и владеет своим apt inventory.
        for job_identifier in ("tests", "coverage"):
            # Subtest сохраняет точное имя job при выпадении любого native dependency.
            with self.subTest(job_identifier=job_identifier):
                # Job извлекается отдельно, чтобы package одной VM не маскировал другую.
                workflow = self.coverage_workflow if job_identifier == 'coverage' else self.ci_workflow
                all_target_job = extract_job(workflow, job_identifier)
                # Exact job-level env не позволяет полному DWARF снова переполнить runner.
                self.assertIn(
                    EXPECTED_ALL_TARGET_TEST_PROFILE_DEBUG,
                    all_target_job.splitlines(),
                )
                # Именованный step является единственным владельцем native inventory этой VM.
                install_step = extract_named_step(
                    all_target_job,
                    "Install native build dependencies",
                )
                # Exact set закрепляет SoundTouch/lavapipe и запрещает package creep.
                self.assertEqual(
                    EXPECTED_ALL_TARGET_NATIVE_PACKAGES,
                    apt_install_packages(install_step),
                )

    def test_cros_libva_workflow_compiles_against_real_pre_and_post_1_23_headers(self) -> None:
        """Две distro jobs компилируют production constructor по обе стороны ABI boundary."""

        # Existing patch matrix на Noble остаётся реальным older-header compile gate.
        legacy_job = extract_job(self.ci_workflow, "dependency-patch-tests")
        # Focused assertion не исполняется для остальных standalone forks.
        legacy_header_step = extract_named_step(
            legacy_job,
            "Assert legacy VA-API 1.20 headers",
        )
        # Условие сохраняет общий matrix без ложных требований к non-libva entries.
        self.assertIn("if: matrix.patch == 'cros-libva'", legacy_header_step)
        # Header version должна быть доказана до запуска bindgen.
        self.assertIn("VA_MINOR_VERSION[[:space:]]+20$", legacy_header_step)
        # Негативная проверка подтверждает отсутствие обоих post-1.22 fields.
        self.assertIn("(seg_id_block_size|va_reserved8)", legacy_header_step)
        # Production crate test, а не helper-only test, компилирует старый struct literal.
        self.assertIn(
            'cargo test --manifest-path "${{ matrix.manifest }}" --locked',
            legacy_job,
        )

        # Resolute job отдельно исполняет cfg `libva_1_23_or_higher`.
        current_job = extract_job(self.ci_workflow, "cros-libva-new-headers")
        # OS pin связан с distro libva 2.23 package и не использует floating ubuntu-latest.
        self.assertIn("runs-on: ubuntu-26.04", current_job)
        # Preview runner получает exact Rust независимо от содержимого tool cache.
        self.assertIn(f"- uses: {RUST_TOOLCHAIN_ACTION}", current_job)
        # Standalone crate не тащит несвязанные audio/FFmpeg/GBM prerequisites.
        current_install_step = extract_named_step(
            current_job,
            "Install cros-libva native build dependencies",
        )
        # Exact focused inventory сохраняет новый gate быстрым и понятным.
        self.assertEqual(
            EXPECTED_CROS_LIBVA_NATIVE_PACKAGES,
            apt_install_packages(current_install_step),
        )
        # Положительные header assertions доказывают обе новые bindgen fields.
        current_header_step = extract_named_step(
            current_job,
            "Assert VA-API 1.23 headers",
        )
        # API minor соответствует доказанной upstream границе.
        self.assertIn("VA_MINOR_VERSION[[:space:]]+23$", current_header_step)
        # Каждое поле проверяется отдельно, чтобы одно не маскировало отсутствие другого.
        self.assertIn("seg_id_block_size;", current_header_step)
        # Точный размер reserved byte array является частью C layout.
        self.assertIn("va_reserved8\\[3\\];", current_header_step)
        # Полный standalone test запускает build.rs, bindgen и production VP9 constructor.
        self.assertIn(
            "cargo test --manifest-path crates/cros-libva-patch/Cargo.toml --locked",
            current_job,
        )


# Стандартный entry point позволяет запускать этот contract отдельно от discovery suite.
if __name__ == "__main__":
    # unittest владеет выводом и ненулевым exit code при нарушении workflow.
    unittest.main()
