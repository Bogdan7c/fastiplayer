"""Focused regression для полного inventory dependency audit."""

# re извлекает именованный Bash-массив без исполнения CI script-а.
import re
# shlex разбирает shell tokens с теми же кавычками, что использует Bash.
import shlex
# pathlib вычисляет стабильные пути относительно текущего test-файла.
from pathlib import Path
# tomllib читает канонический workspace inventory из root Cargo.toml.
import tomllib
# unittest предоставляет hermetic stdlib runner проекта.
import unittest

# Корень репозитория находится на два уровня выше scripts/tests/.
REPO_ROOT = Path(__file__).resolve().parents[2]


# Тест не позволяет явному cargo-machete scope снова отстать от workspace.
class DependencyAuditInventoryTests(unittest.TestCase):
    # Все workspace members должны попасть в audit ровно один раз, а patch crates — ни разу.
    def test_cargo_machete_inventory_matches_workspace_members(self):
        # Root manifest является единственным владельцем workspace membership/exclusions.
        with (REPO_ROOT / "Cargo.toml").open("rb") as manifest_file:
            # tomllib сохраняет точные относительные paths без shell-нормализации.
            workspace_manifest = tomllib.load(manifest_file)
        # CI script читается как данные, поэтому тест не запускает cargo-deny/machete.
        ci_script = (REPO_ROOT / "scripts" / "ci-checks.sh").read_text(encoding="utf-8")
        # Именованный readonly array образует устойчивую границу для regression test-а.
        inventory_match = re.search(
            # Non-greedy body заканчивается на закрывающей строке exact массива.
            r"readonly -a WORKSPACE_CRATE_DIRECTORIES=\(\n(?P<body>.*?)\n\)",
            # DOTALL разрешает одному выражению покрыть многострочный Bash array.
            ci_script,
            # Флаг сохраняет выражение компактным и не меняет shell semantics.
            flags=re.DOTALL,
        )
        # Переименование/удаление boundary должно давать понятный focused failure.
        self.assertIsNotNone(inventory_match)
        # Type narrowing безопасен после assert и сохраняет дальнейший код читаемым.
        assert inventory_match is not None
        # shlex выдаёт exact path tokens, которые получит cargo-machete.
        audited_directories = shlex.split(inventory_match.group("body"), comments=True)
        # Повтор одного crate скрывал бы ошибку review и создавал лишнюю работу audit-а.
        self.assertEqual(len(audited_directories), len(set(audited_directories)))
        # Members root manifest-а являются полным ожидаемым first-party набором.
        workspace_directories = set(workspace_manifest["workspace"]["members"])
        # Exact equality ловит как пропущенный новый crate, так и stale directory.
        self.assertEqual(set(audited_directories), workspace_directories)
        # Standalone patch crates намеренно проверяются отдельным integration boundary.
        excluded_directories = set(workspace_manifest["workspace"]["exclude"])
        # Рекурсивный захват patch crate-а снова дал бы false-positive dependencies.
        self.assertTrue(set(audited_directories).isdisjoint(excluded_directories))


# Прямой запуск файла остаётся удобным вне общего unittest discovery.
if __name__ == "__main__":
    # unittest управляет exit status для shell/CI.
    unittest.main()
