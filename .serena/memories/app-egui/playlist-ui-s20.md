# Session 20 — Playlist row interaction UI (2026-07-16)

## UI ownership
- `ui/playlist/actions.rs` остаётся единственным UI→post-render boundary. Row actions: exact `Select(ItemId)`, `Play(ItemId)`, `Remove(ItemId)`, `RemoveOthers(ItemId)` и один canonical `Move { item_id, MoveItemIntent }`.
- `ui/playlist/row_interactions.rs` владеет только egui click/double-click, focused keyboard и context-menu mapping. Single click выбирает без Play; double click и Enter создают explicit Play; Up/Down/Home/End выбирают и переносят focus; Delete удаляет exact focused row. Keys потребляются только при row focus, поэтому global hotkeys Session 18A не дублируются.
- Context menu вызывается на click-sensing `Response` каждый frame и содержит русские Play/Remove/Remove Others. Duplicate locator rows адресуются только stable Item ID.
- После successful removal `playlist_action_runtime` передаёт в `AppState::request_playlist_row_focus` только controller-provided D47 selected Item ID. UI не вычисляет fallback по virtualized index и не хранит Undo snapshot. Active/pending changes D61 сами focus/viewport не меняют.

## Virtualized drag
- `ui/playlist/virtualized_drag.rs` владеет ephemeral `VirtualizedDragState`: source Item ID, capture generation, pointer/viewport/scroll geometry, requested scroll offset и stable insertion target. Widget/index references не хранятся; source row может стать невидимым.
- Pinned egui 0.34.2 contract: `drag_started`/transport release flags одно-frame; built-in `DragAndDrop` payload доступен в release frame и автоматически очищается на Escape/release. Implementation дополнительно очищает playlist state и собственный matching payload при viewport leave, disappearance и lost capture; replacement payload другого UI owner сохраняется.
- Insertion slot вычисляется O(1) из pointer + scroll offset через `PlaylistViewModel::item_id_at` и преобразуется в ToFront/ToBack/Before(anchor). Drop публикует ровно один Move action; pointer motion domain не мутирует.
- Edge repaint запрашивается только в scrollable edge zone. Leave/center/content boundary/drop/cancel прекращают explicit repaint, поэтому idle spin нет.
- Renderer остаётся fixed-height `ScrollArea::show_rows`; standard egui Frame/SelectableLabel используются для interaction/accessibility/insertion marker. Прямого Painter в app-egui нет, `ui-artwork-egui` не менялся.

## Verification
- Focused Playlist UI: 24 PASS, включая click-vs-double, exact duplicate IDs, keyboard/context, first/middle/last/off-screen 10k target, owned/foreign-payload cleanup и edge start/stop.
- Default-feature focused playlist: 211 PASS; sidebar: 13 PASS; full `app-egui --no-default-features`: 570 PASS; strict app Clippy, fmt, Rust 1.96 locked workspace check, refactor guardrails и diff check PASS.
- Handoff: `user/playlist_queue_implementation_plan.md`. Следующая разрешённая сессия — 21.
