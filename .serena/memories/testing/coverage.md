# Stable coverage v2 (authoritative, 2026-08-30)

Этот документ полностью заменяет прежние Session 07B/28/S42 утверждения об aggregate v1 baseline и одном measured run. Историческая модель сохранена только внутри `coverage/baseline.json.legacy_report_only` как provenance и никогда не участвует в blocking decision.

## Владельцы и tracked policy

- `scripts/coverage.sh` — единственный shell entrypoint: `check` проверяет tracked v2 baseline, `bootstrap <proposal>` создаёт review-only proposal и не меняет tracked policy.
- `scripts/coverage_stability.py` + `scripts/coverage_stability_schema.py` владеют source-coordinate run/cohort/baseline schemas, stable ratchet, atomic baseline-update policy и measurement-exception lifecycle.
- `coverage/baseline.json` — schema v2, exact audited bootstrap proposal SHA-256 `e2adde1f9badab2fee2a8449e399c46d4c9589f1fb39496cc7044629df9c6e17`.
- `coverage/measurement-exceptions.json` — единственный blocking exception ledger, schema v1; initial exact empty ledger SHA-256 `1f64ad40d0db9ebf1a108da65cd02c8baec6a26c41e78e85add972c6f3534a2b`.
- `coverage/exceptions.json` и embedded v1 baseline — frozen `legacy_report_only` provenance. Они не разрешают v2 regression и не являются параллельным источником истины.
- `coverage/policy.json` по-прежнему классифицирует blocking/informational crates. `coverage/executable-inventory-policy.json` типизированно разрешает runtime-built root только для `settings-derive/tests/trybuild`.

## Методология

1. Один `cargo test --no-run` materializes parent executable cohort.
2. Exact stale runtime root transactionally quarantined. Typed cargo-test prewarm materializes trybuild; prewarm profiles удаляются.
3. Source tree, parent executable inventory и runtime trybuild inventory freeze-ятся до measured run 1. Build/runtime semantic identity — logical path + mode + size + SHA-256; symlink/root escape/add/remove/mode/content change fail closed. Parent metadata optimization может пропускать повторный hash только после runtime ctime probe; mtime/inode сами по себе не semantic.
4. Ровно три measured workspace runs выполняются с обычной concurrency. Source coordinate классифицируется: 3/3 — stable; 1/3 или 2/3 — variable diagnostic; 0/3 — uncovered. Blocking ratchet использует stable coordinates, а variable remains visible report-only.
5. Run/cohort/artifact publication transactional. Failure restores previous merge metadata and quarantined runtime root; partial stage/prewarm/run1/run2 profiles не публикуются. Successful cohort retains exact authoritative run3 profraw set + profdata/list for report artifact.
6. Runtime builder policy, source identity, toolchain identity, build/runtime inventories and every published artifact are recorded in schema-v2 cohort manifest. New runtime owner fails until explicitly modeled.

Bootstrap accepted on Rust 1.96.0, LLVM 22.1.2 and cargo-llvm-cov 0.8.7:
- source: 2,276 files;
- parent executables: 287 logical paths / 213 unique identities;
- typed trybuild runtime: 13 logical paths;
- final run3 profiles: exact 155;
- workspace stable: functions 15,115/19,430, lines 155,646/204,112, regions 196,198/260,067;
- blocking stable: functions 9,699/11,586, lines 97,554/114,300, regions 122,380/146,575.

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
