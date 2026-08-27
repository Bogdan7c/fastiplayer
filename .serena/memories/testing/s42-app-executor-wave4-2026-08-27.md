# S42 app-egui production boundary splits — executor wave 4 (2026-08-27)

## Поведение и API

- Это behavior-neutral relocation: существующие `pub(crate)`/`pub(super)` пути, inherent methods, typed outcomes, ordering и error semantics сохранены private re-export-ами.
- `crates/app-egui/src/video_pipeline_candidate/protocol.rs` теперь содержит passive vocabulary staged candidate protocol-а: `RendererGeneration`, data-only staged candidate/state, diagnostics и typed terminal/match/status/cancel outcomes. Parent `video_pipeline_candidate.rs` по-прежнему владеет player port, slot mutation, terminal cleanup/release, post-`Installed` commit, active pointers и `PreparedPostInstalledVideoPipelineCommit::Drop`.
- `crates/app-egui/src/settings_runtime/route_apply/snapshots.rs` содержит immutable committed renderer/player/media snapshots и чистые projections player reports/results, включая test-host report. Parent `route_apply.rs` сохраняет transaction preflight, apply, rollback, finalize и все вызовы runtime owners.
- `crates/app-egui/src/playlist_runtime/controller/install/state.rs` содержит typed install phases/requests/outcomes, state payloads и только read-only projections `install_phase`, `install_request_id`, `accepts_playback_intent_update`. Parent `install.rs` сохраняет все transitions, reservation/token operations и authority порядка Ready → authorization → Installed.

## Инварианты

- Candidate resource halves не получают нового release/commit owner-а; post-Installed token Drop по-прежнему восстанавливает commit-required candidate.
- Settings busy/conflict/failure/partial-failure distinctions, snapshot commit-after-owner-success и compensating rollback не менялись.
- Playlist reservation не освобождается до прежней terminal boundary; exact request/player-request/intent revisions и Ready → authorize → Installed fencing не менялись.
- Новых public или crate-wide API не добавлено; child visibility ограничена ровно прежними parent scopes.

## Размеры и проверки

- Production line counts parent/child: candidate `755/215`; settings `646/296`; playlist install `777/222`.
- Focused functional PASS: candidate lifecycle/release `15/15`; settings transaction/runtime `48/48`; playlist controller/install `107/107`.
- Full app PASS: no-default `1000/1000`; all-features `1000/1000`.
- Strict app Clippy PASS для no-default и all-features/all-targets с `-D warnings`; app no-default check PASS; full rustfmt check, full diff check и refactor guardrails PASS.
- S42 final acceptance `24/24` PASS; Serena diagnostics clean для всех шести parent/child файлов после reactivation.
- Global `scripts/check_s42_guardrails.py` остаётся red из-за repository-wide stale/legacy baseline inventory. Для трёх app parent paths он сообщает только ожидаемый stale baseline после уменьшения; ни один новый child не превышает hard limit. Baseline намеренно не менялся.
