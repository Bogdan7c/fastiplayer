# S18 — Playlist/topology hardening gate (2026-07-21)

Связано с `mem:core`, `mem:playlist/core`, `mem:playlist/state`, `mem:app-egui/playlist-ui-s20` и `mem:media-services/core`.

## Verdict

- Milestone playlist/topology PASS без новой feature logic и без production Rust/API/dependency-boundary изменений.
- Existing focused suites доказали M3U/M3U8/XSPF/CUE, nested budgets/cycles, import/export/confirmations, compound navigation/shuffle/Undo/UI, compound MPRIS, yt-dlp Video/Playlist/MultiVideo/Delegation, URL Append-only, detached interactive Replace, startup/desktop StartupReplace, schema v2 и отдельный resume sidecar.
- Canonical owner остаётся `PlaylistQueue::Vec<PlaylistEntry>`; playable part traversal derived и не хранится вторым canonical Vec. Persistence v2, export, ordinary/compound UI view и external projection закреплены source guardrail-ами на top-level entry API.
- Legacy `PlaylistQueue::items()` и ambiguous queue `len()` остаются запрещены; workspace audit теперь включает появившийся позже `playlist-io`.
- Presentation secret audit запрещает intent-named raw secret/payload exposure в Playlist UI, app external projection и `desktop-integration`. Existing redaction tests остаются functional evidence.
- M3U/XSPF format-specific source aliases сохранены после reference audit: они имеют живые production/test callsites. Ломающего alias cleanup в gate нет.

## Guardrails и coverage inventory

- `scripts/check-refactor-guardrails.py` теперь фиксирует `web-media-core` как required std-only neutral contract и проверяет canonical structural consumers + presentation secret boundary.
- Focused Python tests проверяют pass/fail dependency, flattening и raw-secret cases.
- `coverage/policy.json` классифицирует ранее пропущенные `atomic-file-store`, `bounded-xml-reader`, `playlist-io` и `web-media-core` как blocking neutral crates. Baseline/exceptions не переписывались: известный repository-wide coverage relocation blocker из `mem:testing/coverage` остаётся отдельной задачей.

## Test hardening

- Full workspace sweep выполнил все target-ы и обнаружил один старый scheduler-dependent assert в `playlist-state::worker::tests::wake_is_coalesced_until_drain_and_terminal_report_is_exactly_once`.
- Корень: ожидание первого wake не создаёт happens-before для следующего `publish_warning`; production mailbox contract обещает bounded/coalesced events, а не завершение обеих последовательных публикаций к моменту пробуждения test thread.
- Тест теперь проверяет mailbox policy напрямую: attempt + warning публикуются до drain, делят один outstanding wake и выдаются exactly once. Production worker/mailbox код и semantics не менялись.

## Verification

- `scripts/ci-checks.sh tests`: все workspace target-ы прошли кроме найденного старого flaky assert; после исправления полный `cargo +1.96.0 test -p playlist-state --all-features` — 48 PASS.
- В том же workspace sweep: app-egui 805 PASS, playlist-core 128 PASS, playlist-io full integration matrix PASS, service-ytdlp/topology PASS и остальные workspace/doc tests PASS.
- `scripts/ci-checks.sh format-guardrails`: PASS, включая 33 guardrail tests, coverage inventory, toolchain/patch policy, scripts и rustfmt.
- strict Clippy для playlist-core/playlist-state, Rust 1.96 locked all-feature workspace check, focused MSRV 1.92 check, `git diff --check` и Serena diagnostics: PASS.
