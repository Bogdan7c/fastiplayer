# S17G compound runtime view model/actions (2026-07-21)

Связано с `mem:core`, `mem:app-egui/playlist-ui-s20`, `mem:app-egui/playlist-controller-s20`, `mem:app-egui/wake-runtime-s10a` и `mem:playlist/compound-hardening-s01g-2026-07-20`.

## Ownership и boundaries

- Единственный authoritative owner остаётся `PlaylistRuntime -> PlaylistController`. Controller владеет queue/current, structural selection и process-lifetime disclosure state. Renderer authority не получил.
- Новый renderer-neutral модуль: `crates/app-egui/src/playlist_runtime/compound_view.rs`; focused tests вынесены в `compound_view/tests.rs`, чтобы production module остался меньше 700-800 строк.
- `CompoundRuntimeViewSnapshot` содержит top-level `Single` / `CompoundHeader` и только для expanded group — subordinate `CompoundPart` projections.
- `top_level_entry_count` является domain count, `visible_row_count` — presentation count. Child projections не queue entries.
- Disclosure хранится в `CompoundRuntimeViewState` как set group IDs, process-lifetime/UI-only; при structural commit удаляются только исчезнувшие groups.

## Identity contracts

- Structural selection полностью переведён на `PlaylistEntryId`: `selected_entry_ids`, range anchor, interaction cursor, keyboard/D47/Undo focus и stable egui row identity. Production compatibility с selected Item ID отсутствует; старые helpers ограничены `#[cfg(test)]`.
- Queue `traversal_current` остаётся exact `PlaylistItemId`, в том числе subordinate part.
- Existing collapsed/top-level `PlaylistViewSnapshot` строит одну row на entry; все part Item IDs индексируются к group header.
- `CompoundRuntimeRow::CompoundPart` не предоставляет structural entry identity (`structural_entry_id() == None`), поэтому child нельзя превратить в remove/reorder/drag.
- remove-selected, remove-unselected, multi-move и selected export scope принимают/разрешают exact top-level `PlaylistEntryId`; прежний part-to-group inference удалён. Explicit `PlaylistEntryId::Single(part_id)` subordinate target отвергается.
- Removal/Undo outcomes возвращают `selected_entry_id`, поэтому restored focus группы указывает на header, а не неявно на first part.

## Typed compound actions

- `ToggleCompoundDisclosure`, `CompoundHeaderPlayAction`, `CompoundPartPlayAction` несут structural revision; stale actions отвергаются до mutation/open.
- Group actions требуют explicit `PlaylistEntryId::Compound`. Header action с `Single` возвращает `NotCompoundEntry`.
- Header Play разрешает ровно один target: current part, если current внутри group, иначе first part. Нет fallback candidate list и hidden sequential scan после failed first.
- Part Play проверяет explicit group membership, затем вызывает существующий exact strong-open boundary один раз и не меняет selection/anchor/disclosure.
- Current Item target typed: collapsed group -> `Header(PlaylistEntryId)`, expanded -> `Part(PlaylistItemId)`; auto-expand отсутствует.

## Publication и следующий этап

- Controller публикует отдельный `Arc<CompoundRuntimeViewSnapshot>` вместе с обычным view snapshot и после disclosure toggle.
- Compound runtime adapters уже добавлены в `row_interactions.rs`.
- egui rendering/accessibility expanded children намеренно не подключены в S17G; это boundary следующей сессии S17V. До S17V staging APIs имеют local `dead_code` allowance с причиной.

## Проверки

Focused tests покрывают collapsed/expanded one/many, active collapsed summary/exact active child, header current/first/no-scan contract, collapse-independent Single/Compound range anchors, part click selection preservation, Current Item target и stale/wrong-group actions. Existing removal/reordering/export/Undo/UI tests мигрированы на entry identities.

Успешные проверки:
- `cargo test -p app-egui --all-features`: 788 passed;
- `scripts/ci-checks.sh tests`: workspace tests и doc-tests passed (повторный полный прогон);
- `cargo clippy -p app-egui --all-features --all-targets -- -D warnings -A clippy::large_enum_variant`: passed; allowance относится только к двум pre-existing strong-media-open enum warnings;
- `scripts/check-refactor-guardrails.py`: OK;
- `cargo fmt --all -- --check` и `git diff --check`: passed.

Serena per-file diagnostics для новых core files чистые. После cross-file focus type migration rust-analyzer внутри Serena показывал stale errors в трёх consumer files, хотя fresh cargo test/clippy полностью их скомпилировали; source owner files diagnostics clean.
