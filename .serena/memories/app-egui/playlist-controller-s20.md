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
