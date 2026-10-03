# Coverage-gate УДАЛЁН (решение владельца 2026-10-03)

Вся инфраструктура покрытия удалена из репозитория. Подробная история (stable v2, 3 прогона, baseline, ledger, patch-coverage серия) — только в git (`git log -- coverage/ scripts/coverage.sh`) и в `mem:archive/core-history-2026-05-to-09`.

## Почему
Покрытие доказывает лишь «строка выполнилась», а не «тест проверил результат». Строгий gate поощрял ИИ писать тесты ради координат. Серия patch coverage (`user/coverage-patch-rule/`) отменена.

## Что удалено
- `scripts/coverage.sh`, все `scripts/coverage_*.py`, их тесты (`scripts/tests/test_coverage_*.py`, `coverage_*workflow_contract.py`, `fixtures/coverage_stable/`).
- `coverage/` (baseline, ledger-ы, policy), `.github/workflows/coverage.yml`, CI job `Coverage baseline policy`, `docs/code-coverage.md`, `docs/coverage-qualification-2026-09-05.md`.
- Guardrail `find_existing_demux_coverage_policy_violations` (check-refactor-guardrails), шаг coverage в `scripts/final-acceptance.sh`, coverage-evidence в `roadmap-trace-s42.json` (audit-08 → `audit-08-dependency-inventory`).

## Что осталось
- `scripts/test_execution_scope.py` + `FASTIPLAYER_TEST_SCOPE=hosted` — НЕ coverage: фильтрует аппаратные тесты в обычном CI (`ci-checks.sh`).
- `runtime-coverage-s41.json` (service-ytdlp) — это покрытие профиля провайдеров, к gate-у отношения не имеет.

## Как теперь
- Качество тестов — правило в `AGENTS.md`: «тест проверяет результат, а не вызов».
- Посмотреть покрытие для себя: `cargo llvm-cov --workspace --all-features --html` (не gate).
- Идея на будущее (не решено): mutation testing изменённого кода `cargo mutants --in-diff` в режиме отчёта.
- Не восстанавливать coverage-gate и не писать тесты «ради покрытия» без нового решения владельца.
