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


## Full-row desktop multi-select UI revision (2026-07-18)
- Each virtualized row lays out non-selectable child labels, then registers exactly one full-width `Sense::click_and_drag` response. Index, media glyph, title, duration, badges and trailing edge therefore share click/double-click/right-click/hover/drag semantics; system text selection is disabled.
- UI actions are intent-shaped: `UpdateSelection`, `RemoveSelected`, `RemoveUnselected` and `MoveItems`, with exact Arc-backed IDs and captured structural revision. Normal/Ctrl-or-Cmd/Shift/Ctrl-or-Cmd+Shift pointer selection, focused arrows/Home/End, Ctrl-or-Cmd+A, Escape, Enter and Delete follow desktop semantics without consuming global hotkeys outside row focus. Empty list background clears selection; Escape during drag cancels drag only.
- Context click inside selection preserves the group; outside selection first selects the row. Double click preserves an existing group but plays the exact clicked row. Context menu exposes Play, Remove selected (N), and Remove all except selected (N); full queue scans happen only when an explicit range/select-all/bulk/drag-start event needs exact IDs, not during ordinary row rendering.
- Virtualized drag captures selected IDs and structural revision once. Dragging an unselected row first collapses selection to it; selected rows move atomically as one canonical-order block. Drop slots exclude selected anchors, per-frame target lookup stays O(1), and Escape/lost capture clean ephemeral state.
- Row visuals come only from `PlaylistRowStyle`: white alpha 28 hover, 46 selected, 64 selected+hover, white alpha 128 separator and light grayscale insertion/focus/active strokes. Playing remains independent through `▶`, active fill/stroke and accessibility selected state.
- Headless egui tests cover every row column/edge plus double click, secondary click, drag/drop, keyboard focus/navigation/select-all, empty-area clear and disabled text selection. Focused Playlist UI 34 PASS; full app suites 643 PASS for both feature sets.
