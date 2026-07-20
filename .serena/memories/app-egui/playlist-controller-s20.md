# Session 20 — row Play/reorder controller boundaries (2026-07-16)

## Play и removal adapters
- `PlaylistRuntime::play_playlist_row` supersede-ит D79 replacement confirmation и делегирует existing `PlaylistController::play_item` с origin UI. Новый navigation/install state не создан.
- `transport_runtime::apply_playlist_row_play` исполняет existing typed D59/D60 outcomes: clean active → exact RestartFromBeginning(StartPlaying) без reopen; runtime playback failure → обычный StartInstall; matching pending → D52 stable-intent update без второй preparation; guarded intent остаётся controller-owned.
- Remove/Delete/RemoveOthers входят только через process-lifetime `PlaylistRuntime` removal boundary. D46 kind/snapshot/8-second Undo и D14a tombstone остаются прежними. Runtime outcome возвращает D47 selected Item ID UI; UI restore/fallback отсутствует.

## Canonical reorder
- Новый `playlist_runtime/controller/reordering.rs` — controller boundary над `PlaylistQueue::move_item`. Он заранее проверяет fatal/dirty/structural revisions, сохраняет domain distinctions (not found, stale anchor, install linearizing, exhausted), и только `Moved` публикует одну controller structural + dirty revision, invalidates manual navigation и rebuild-ит view.
- AlreadyInPlace/self/adjacent target — typed no-op без dirty/persistence. Runtime публикует committed snapshot и инвалидирует D46 Undo только после реального move.
- Canonical move не меняет selected, active, pending, traversal current или current shuffle history/cursor/upcoming; identity остаётся Item-ID based. Core D14 test и focused controller test фиксируют unchanged shuffle snapshot.
- Drag geometry и insertion target не входят controller/domain; controller получает только exact source Item ID + named `MoveItemIntent` на drop.

## Verification
- Focused controller reorder: 2 PASS; existing D59/D60 controller tests PASS внутри full 570 app suite; playlist-core 75 PASS.
- Strict app Clippy, fmt, Rust 1.96 locked workspace check, guardrails и diff check PASS.
- Следующая разрешённая playlist session — 21 hardening; broad hardening в Session 20 не выполнялся.


## Full multi-selection and exact bulk/group actions (2026-07-18)
- `PlaylistController` now owns process-lifetime, non-persistent `PlaylistSelectionState`: an Arc-backed `HashSet<PlaylistItemId>`, range anchor and interaction cursor. Selection is independent from playback/current/pending, dirty revisions and persistence; view snapshots share the set and visible-row `is_selected` is O(1).
- `UpdateSelection` is the only selection mutation boundary. Replace/Toggle/ReplaceRange/AddRange/SelectAll/MoveCursor carry stable IDs and structural revision where needed; Clear has an explicit cursor policy. Range and Select All payloads are revalidated against authoritative canonical order, so stale/partial captures cannot mutate state.
- `remove_selected_items` and `remove_unselected_items` accept exact Arc-backed IDs plus structural revision, validate current selection/complement, and delegate to one existing `PlaylistQueue::remove_batch` commit. One boxed successful removal outcome carries full selection-before/after snapshots into the existing eight-second Undo; Undo restores the complete selection and cursor. Selected removal focuses the nearest survivor.
- `move_items` is the controller/runtime boundary over core group reorder. A real move advances exactly one controller structural+dirty revision, invalidates manual navigation and Undo once, while no-op/error distinctions remain typed. Reorder/sort/append preserve stable-ID selection and do not rebuild its Arc when membership is unchanged.
- Verification: full `app-egui` suite PASS with default and no-default features (643 each); focused removal 17 PASS; strict Clippy and all final guardrails PASS.

## S01Q queue read-boundary migration (2026-07-20)
- Controller selection/removal/reordering/discovery callers больше не читают contiguous `PlaylistQueue` slice. Exact committed membership и order читаются через `iter_playable_ids()`/`iter_playable_items()`, lookup остаётся stable-ID `item()`; structural selection counts используют `top_level_entry_count()`, capacity использует `retained_item_count()`.
- Shift-range validation сохраняет exact structural revision guard и stable-ID payload, а canonical span проверяет через bounded `skip/take`; stale/duplicate/non-contiguous payload остаётся atomic no-op. Nearest-survivor index — только локальная позиция owner turn, которая после successful removal выбирает stable ID и не участвует в queue mutation authority.
- D08 reservation, dirty/controller revisions, selection Arc sharing, Undo, playback/current/pending identities и queue observable behavior не менялись. Full app 719 tests, strict Clippy/check/MSRV/guardrails PASS.
