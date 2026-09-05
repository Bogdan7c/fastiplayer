# AUD-007 bounded single-item yt-dlp output and JSON DOM (2026-08-23)

## Подтверждённая причина

- До исправления candidate/recovery path в `crates/service-ytdlp/src/process.rs` использовал unbounded `Vec::read_to_end` для stdout/stderr, затем создавал `String` и полный `serde_json::Value` DOM. Timeout не ограничивал bytes.
- Независимый RSS profile: 10+10 MiB -> ~25.6 MiB RSS, 100+100 -> ~207.6 MiB, 500+500 -> ~1007.7 MiB; результат всегда `Timeout`, typed overflow отсутствовал.
- Compact valid JSON из маленьких nodes усиливал память DOM примерно до 34x: 8 MiB input -> ~267.5 MiB RSS.

## Архитектура и defaults

- Новый owner `crates/service-ytdlp/src/process_output.rs` владеет single-item output/structure invariant. `process.rs` только оркестрирует existing `OwnedProcess` lifecycle.
- Независимые defaults в `fastiplayer_config::YtDlpConfig`: stdout 64 MiB, stderr 8 MiB, JSON values 1,000,000.
- Реальный corpus system `yt-dlp 2026.08.19` с production args: maximum 826,324 bytes / 11,857 JSON values; defaults дают примерно 81x/84x запас для исследованных обычных, music, multilingual, 4K/8K и lyrics-heavy single-item cases.
- Settings paths: `yt_dlp.single_item_stdout_limit_bytes`, `yt_dlp.single_item_stderr_limit_bytes`, `yt_dlp.single_item_json_node_limit`. Config maxima: 1 GiB, 64 MiB, 10,000,000.
- `YtDlpConfig::validate()` доступен отдельно от полного `AppConfig`; `YtDlpProcessOutputBudgets::from_config` не позволяет direct service caller-у обойти config maxima.

## Runtime semantics

- Stdout/stderr читаются одновременно через `limit + 1`. Shared first-writer-wins atomic signal сохраняет identity первого overflow.
- Stdout удерживается только до configured budget; stderr payload вообще не хранится, сохраняется bounded byte count.
- Typed public errors: `StdoutLimitExceeded { limit_bytes }`, `StderrLimitExceeded { limit_bytes }`, `JsonNodeLimitExceeded { limit_nodes }`.
- При pipe overflow process owner вызывает прежний `OwnedProcess::finish`: owned group прекращается, root reap-ится, reader workers bounded завершаются.
- Перед DOM выполняется allocation-free serde visitor, считающий каждый JSON value; syntax error ниже budget остаётся `InvalidExtractorResponse`. Затем используется `serde_json::from_slice`, без промежуточного `String`.

## Post-fix evidence

- 10+10 MiB: ~15.6 MiB RSS, `StderrLimitExceeded` на 8 MiB.
- 100+100 MiB: ~69.5 MiB RSS, `StdoutLimitExceeded` на 64 MiB.
- 500+500 MiB: ~67.6 MiB RSS, `StdoutLimitExceeded` на 64 MiB.
- Valid compact-node 8 MiB: ~11.6 MiB RSS и `JsonNodeLimitExceeded` до DOM.
- Valid 32 MiB big-string JSON: ~47.7 MiB RSS, проходит process -> DTO -> normalization -> accepted candidate.
- Exact-boundary и limit+1 tests покрывают оба pipe-а; JSON exact/overflow/invalid syntax, descendant group stop, direct-config validation bypass, additive schema-v7 defaults и full candidate normalization также покрыты.
- Verification: config 91/91, settings 17/17, service-ytdlp 149 unit + integration suites, app-egui 950/950, strict touched-crate Clippy, workspace all-targets check, rustfmt, refactor guardrails, diff-check и Serena diagnostics PASS.

## Known limitation

- Single-item budgets ограничивают pipe output и metadata DOM, но не общую RSS самого yt-dlp или будущих headless/browser subprocess-ов. Process-wide cgroup/rlimit требует отдельного browser corpus и решения владельца.
- Playlist/topology path сохраняет собственные streaming line/entry/depth budgets; single-item DOM profile не переносится туда вслепую.

Related: `mem:media-services/ytdlp-process-owner-2026-08-05`, `mem:media-services/ytdlp-system-compatibility-aud006-2026-08-23`, `mem:core`.