# Stable coverage v2 (authoritative, 2026-08-30)

Этот документ полностью заменяет прежние Session 07B/28/S42 утверждения об aggregate v1 baseline и одном measured run. Историческая модель сохранена только внутри `coverage/baseline.json.legacy_report_only` как provenance и никогда не участвует в blocking decision.

## Владельцы и tracked policy

- `scripts/coverage.sh` — единственный shell entrypoint: `check` проверяет tracked v2 baseline, `bootstrap <proposal>` создаёт review-only proposal и не меняет tracked policy.
- `scripts/coverage_stability.py` + `scripts/coverage_stability_schema.py` владеют source-coordinate run/cohort/baseline schemas, stable ratchet, atomic baseline-update policy и measurement-exception lifecycle.
- `coverage/baseline.json` — schema v2. Текущий tracked baseline квалифицирован точным пересечением трёх независимых cohort-ов (9 measured workspace runs) на одной source revision; raw SHA-256 `090f220611fdc3c7cb0dc0911d320b09a5b49743221a71de84fc076d5949ecc7`, logical `sha256:0f769c46fd4f5b5d239c8628cb398cb92a0e122525bea309cbe0e22433d34785`. Он принят без measurement exceptions и подтверждён двумя свежими обычными `scripts/coverage.sh check` с пустыми `regressions`/`universe_changes`.
- `coverage/measurement-exceptions.json` — единственный blocking exception ledger, schema v1; initial exact empty ledger SHA-256 `1f64ad40d0db9ebf1a108da65cd02c8baec6a26c41e78e85add972c6f3534a2b`.
- `coverage/exceptions.json` и embedded v1 baseline — frozen `legacy_report_only` provenance. Они не разрешают v2 regression и не являются параллельным источником истины.
- `coverage/policy.json` по-прежнему классифицирует blocking/informational crates. `coverage/executable-inventory-policy.json` типизированно разрешает runtime-built root только для `settings-derive/tests/trybuild`.

## Методология

1. Один `cargo test --no-run` materializes parent executable cohort.
2. Exact stale runtime root transactionally quarantined. Typed cargo-test prewarm materializes trybuild; prewarm profiles удаляются.
3. Source tree, parent executable inventory и runtime trybuild inventory freeze-ятся до measured run 1. Build/runtime semantic identity — logical path + mode + size + SHA-256; symlink/root escape/add/remove/mode/content change fail closed. Parent metadata optimization может пропускать повторный hash только после runtime ctime probe; mtime/inode сами по себе не semantic.
4. Ровно три measured workspace runs выполняются с обычной concurrency. Source coordinate классифицируется: 3/3 — stable; 1/3 или 2/3 — variable diagnostic; 0/3 — uncovered. Blocking ratchet использует stable coordinates, а variable remains visible report-only.
5. Run/cohort/artifact publication transactional. Build, runner или publication failure восстанавливает предыдущие merge metadata и quarantined runtime root; partial stage/prewarm/run1/run2 profiles не публикуются. После успешной публикации полного cohort semantic ratchet выполняется отдельно: его exit 1 намеренно сохраняет новый cohort, variable diagnostics и `check.json` как evidence регрессии, но не меняет tracked baseline/exception ledger и не считается успешным gate. Successful cohort retains exact authoritative run3 profraw set + profdata/list for report artifact.
6. Runtime builder policy, source identity, toolchain identity, build/runtime inventories and every published artifact are recorded in schema-v2 cohort manifest. New runtime owner fails until explicitly modeled.

Текущий independently qualified baseline принят на Rust 1.96.0, LLVM 22.1.2 и cargo-llvm-cov 0.8.7. Его exact workspace stable intersection: functions 15,138/19,448, lines 155,989/204,432, regions 196,739/260,578; blocking stable: functions 9,708/11,594, lines 97,681/114,414, regions 122,587/146,771.

Для baseline update после конкурентных test-only правок один cohort статистически недостаточен. Обязательный human-reviewed qualification workflow: три независимых cohort-а на одной source revision (9 measured workspace runs), exact 9-run stable intersection, file-local audit каждого изменённого файла и два свежих обычных repeatability-check после установки tracked baseline. Aggregate workspace ratio не может скрывать file-local stable loss. CLI пока не автоматизирует cross-cohort reducer; evidence хранится вне tracked policy и сверяется вручную.

Внутри каждого cohort executable logical paths/mode/size/SHA-256 обязаны быть byte-identical. Между независимыми cohort-ами ELF SHA может различаться из-за linker/compiler nondeterminism только если source/tool/profile и coordinate universes совпадают, а каждый cohort отдельно проходит fail-closed manifest validation. Это осознанная граница измерения, не гарантия reproducible build. Реальный test/build failure остаётся failure: retry, measurement exception или ослабление baseline его не легализуют.

## Baseline update policy

PR comparison is one atomic pair:
`python3 scripts/coverage_stability.py check-baseline-update --previous-baseline ... --previous-measurement-exceptions ... --proposed-baseline ... --proposed-measurement-exceptions ...`.

All four inputs are required; command is read-only. Exit 0 = allowed, 1 = well-formed semantic policy violation, 2 = malformed/schema/redaction/I/O failure. Same-universe stable loss cannot be excepted. Cross-universe changes require exact proposed bounded rows; previous rows do not authorize a new decrease. Unknown, stale, overbroad and malformed rows fail closed. Recovery from an expired previous ledger is allowed only by a clean proposed pair, so policy cannot deadlock.

Legacy `coverage_metrics.py check-baseline-update` was deleted; v1 report-only helpers remain for provenance validation only.

## Commands and tests

- Full local/CI gate: `scripts/coverage.sh check`.
- Review-only proposal: `scripts/coverage.sh bootstrap target/coverage/<proposal>.json`.
- Baseline/ledger validation: `python3 scripts/coverage_stability.py validate --kind baseline|measurement-exceptions --input <path>`.
- Focused tests: `scripts/tests/test_coverage_baseline_update.py`, `test_coverage_stability.py`, `test_coverage_metrics.py`, runner/inventory/quarantine/publication suites.
- CI wiring oracle: `scripts/tests/test_s42_release_runner.py::S42ReleaseRunnerTests::test_coverage_check_composes_stable_preflight_suite_and_ratchet`; pure canonical workflow parser: `scripts/tests/coverage_workflow_contract.py`.
- Human contract: `docs/code-coverage.md`; CI overview: `docs/continuous-integration.md`.
