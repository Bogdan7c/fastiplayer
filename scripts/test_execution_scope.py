#!/usr/bin/env python3
"""Явная граница локальных аппаратных тестов и hosted software CI."""

from __future__ import annotations

import os
import subprocess
import sys
from enum import Enum
from typing import Sequence


class TestExecutionScope(str, Enum):
    """Локальная машина проверяет GPU; hosted runner выполняет software suite."""

    LOCAL = "local"
    HOSTED = "hosted"


# Только эти тесты требуют настоящий render node/DMA heap. Fake tests остаются.
LOCAL_HARDWARE_TESTS = (
    "gbm_allocator::tests::test_allocate_gbm_buffer",
    "linear_gbm_frame::safety_tests::frame_keeps_owner_device_alive_and_cpu_mapping_fails_closed",
    "dma_heap::tests::test_allocate_dma_buffer",
    "dma_heap::tests::test_allocate_dma_buffer_1mb",
    "dma_heap::tests::test_dma_buffer_fd_valid",
)


def execution_scope() -> TestExecutionScope:
    """Неизвестное значение не может молча отключить часть qualification."""

    scope = TestExecutionScope(os.environ.get("RUSTIPLAYER_TEST_SCOPE", "local"))
    if os.environ.get("GITHUB_ACTIONS") == "true" and scope is not TestExecutionScope.HOSTED:
        raise ValueError("GitHub Actions requires explicit hosted test scope")
    return scope


def scoped_test_command(
    arguments: Sequence[str], scope: TestExecutionScope
) -> list[str]:
    """Фильтрует libtest до запуска теста, сохраняя сборку и обычную concurrency."""

    command = list(arguments)
    if scope is TestExecutionScope.HOSTED:
        if "--" not in command:
            command.append("--")
        for test_name in LOCAL_HARDWARE_TESTS:
            command.extend(("--skip", test_name))
    return command


def main() -> int:
    """Исполняет переданный Cargo argv без shell, сохраняя failure status."""

    if len(sys.argv) < 2:
        print("usage: test_execution_scope.py CARGO TEST ARGS...", file=sys.stderr)
        return 2
    try:
        return subprocess.run(
            scoped_test_command(sys.argv[1:], execution_scope()), check=False
        ).returncode
    except (ValueError, OSError) as error:
        print(f"test execution scope error: {error}", file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
