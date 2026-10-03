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

## Аудит бесполезных тестов (2026-10-03, после удаления gate)
- Скрининг ~5000 тестов (66 crate-ов; vendored `*-patch` не трогали) + проверка мутациями. Мусора меньше, чем ожидалось: все 12 coverage-тестов от 2026-10-03 (flv, PSI, prefetch, hls limits, redaction, EBML hints, demux ceilings, custom_thresholds, provider id, ValidatedVodMediaPlaylist, Display раскладки каналов) проверяют результат — оставлены.
- Решения владельца: grep-тесты исходников переписывать на поведение, где возможно (UI — headless egui + accesskit), архитектурные guard-ы (запрещённые зависимости/типы) оставлять; пины констант удалять только если есть строгий дубль (дефолты config закрепляет golden `tests/fixtures/current_schema_v10.toml`); passthrough-тесты сворачивать, но per-boundary error-state тесты (правило 7 AGENTS) не сливать; тест с evidence-ссылкой (`preference_distinguishes_*`, runtime-coverage-s41.json) и test-only prod API `MediaInstallFailureStage::ALL`/`MediaInstallCommitPoint` не трогать.
- Приёмы для UI-тестов app-egui: клик — `PointerMoved` → press → release по кадрам; список кнопок — `ctx.enable_accesskit()` + `Role::Button` labels; «клавиша съедена» — `ui.input(key_pressed)` после виджета в том же кадре; выделение текста — `ctx.with_plugin::<egui::text_selection::LabelSelectionState,_>(|s| s.has_selection())`. `Context::style()` в egui 0.34 deprecated → `global_style()`.
- Находка аудита — symphonia `probe` глотала io-ошибку producer-а потокового пути → `UnsupportedFormat`; исправлено 2026-10-04 (`mem:symphonia-demux/core`, раздел «Stream probe failure observer»).

## Как проверять тест мутацией (уроки)
- Мутации делать во временной копии или с немедленным откатом; в worktree — ОТДЕЛЬНЫЙ `CARGO_TARGET_DIR`: общий target с основным деревом оставил в основном дереве устаревшие (мутированные) артефакты, тесты в `main` «падали» на чистом коде до `touch` исходников.
- Serena `replace_symbol_body` для Rust заменяет функцию вместе с атрибутами (`#[test]` пропадает) — после него проверять число тестов (`git grep -c '#\[test\]'`).
