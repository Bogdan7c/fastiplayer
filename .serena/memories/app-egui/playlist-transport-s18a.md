# Playlist transport UI/hotkeys — Session 18A

- `transport_runtime.rs` — единый app adapter для typed `TransportControlAction`; UI и hotkeys всегда задают `TransportActionOrigin::Ui`. Traversal, D17 restart, D50 wait, D53 fast cursor/supersede и stable playback intent остаются controller-owned.
- `ui/player_controls/transport.rs` рендерит стандартные egui Previous/Play-Pause/Next в existing bottom host и возвращает typed actions после render. `PlaylistTransportUiModel` immutable и несёт playlist view revision, Previous/Next availability (`Ready`, `PotentialWait`, `Pending`, `Disabled`), global wait status, Undo status и следующий UI deadline.
- `app_shell/hotkeys.rs` — pure winit 0.30 classifier: Pressed/repeat следует текущей policy; logical `NamedKey::MediaTrackPrevious/Next` проверяется первым, physical `KeyCode::MediaTrackPrevious/Next` — только fallback, один KeyEvent даёт максимум одно action. Hardware media keys работают при egui capture; P/N/Space и legacy keys подавляются при `egui_wants_keyboard_input`/`text_edit_focused`/consumed.
- Play/Pause icon parity централизована в `playback_toggle_will_pause`, а само действие вычисляет toggle из controller-owned `StablePlaybackIntent`, не из transient `PlaybackState`. При playlist active/pending controller создаёт D51/D52 exact/staged dispatch; legacy `TogglePlayback` отправляется только если controller не дал exact/pending route, поэтому повторные staged нажатия меняют stable intent и двойного dispatch нет.
- `state/playlist_transport.rs` хранит bounded renderer state: один active strong request, exact item ID, один latest queued D53 plan, exact transport receipts и intent receipts. Locator/source preparation использует runtime exact plan и service-owned adapters, затем существующий strong D08/D39 protocol; отдельной install state machine нет.
- Pre-admission source/preparation failure обязан вызывать `report_unstaged_manual_navigation_target_failure(item_id)`, чтобы exact preview перешёл в D55, опубликовал view и не завис в Pending. Если superseded request всё же выиграл Installed barrier, queued latest D53 plan запускается как новая regular transaction от уже committed current.
- D80 global status/Cancel и D46 Undo доступны независимо от sidebar. Undo countdown не использует `egui::request_repaint_after`: owner публикует exact deadline, `BackgroundPollScheduler` объединяет его с background deadline и делает один due redraw без idle spin.
- Main review устранил overlap prototype Next с existing playback-rate button, добавил geometry regression, перевёл toggle на stable-intent owner и гарантировал D55 для любого pre-admission start failure; empty receipt polling больше не пишет debug каждый frame.
- Проверки Session 18A: app no-default 540; focused player-controls 31/hotkey 6; desktop-integration 20; strict app clippy no-default/all-targets; fmt; Rust 1.96 locked workspace check; guardrails; diff check. Serena diagnostics чисты в 14 production files; `frame_prepare.rs`/`frame_prepare/ui_prepare.rs` сохранили stale call-site cache старых сигнатур, который опровергнут полным Cargo test, Clippy и workspace check. Handoff: `user/playlist_queue_implementation_plan.md`.


## Session 18B continuation (2026-07-16)
- UI/hotkey transport from 18A is unchanged. MPRIS now enters the same controller boundaries with origin `Mpris`, including D17/D50–D53 traversal, D58 Stop guard and controller-owned Stopped disposition.
- Process-lifetime ownership, modes/volume/identity, MPRIS capability matrix and correlated seek are documented in `mem:app-egui/playlist-desktop-transport-s18b`.


## Shuffle/Repeat перенесены в persistent transport (2026-07-18)

- `PlaylistRuntime`/`PlaylistController` остаются единственным владельцем подтверждённых `shuffle_enabled` и `RepeatMode`; UI не хранит optimistic mode state.
- `PlaylistTransportUiModel` несёт authoritative `shuffle_enabled`, `repeat_mode` и `queue_modes_enabled`. При отсутствующем/остановленном runtime режимы disabled, но selected-состояние модели сохраняется логически.
- `TransportControlAction::{SetShuffleEnabled { enabled }, SetRepeatMode { mode }}` передают точный typed intent. Runtime применяет его через controller startup-record methods; ошибка сохраняет прежний snapshot и публикует безопасную feedback-команду.
- Repeat cycle: `StopAtEnd -> RepeatQueue -> RepeatOne -> StopAtEnd`. Shuffle отправляет точную инверсию текущего authoritative snapshot.
- Старые Shuffle/Repeat полностью удалены из playlist toolbar и `PlaylistInteractionModel`; toolbar сохраняет сортировку, «После текущего» и прочие playlist-команды.
- Layout принадлежит `app-egui`: hit-area 32 pt, glyph 18 pt, preferred distance 156 pt, Shuffle зеркален Repeat, минимум 12 pt Next–Repeat; rate/Next зависят от reveal progress, сами queue-mode rect стабильны.
- Accessibility использует selected button metadata, русские current/next labels, Tab + Space/Enter и pointer focus surrender.
- Focused tests находятся в `crates/app-egui/src/ui/player_controls/queue_mode_controls/tests.rs`; artwork tests — в `crates/ui-artwork-egui/src/lib.rs`.

## Queue-mode availability во время смены трека (2026-07-19)
- D08/D39 install commit-guard не делает Shuffle/Repeat недоступными: `PlaylistController::request_queue_modes` принимает intent во время guarded install и сохраняет ровно одно desired value до commit/abort.
- Поэтому `PlaylistTransportUiModel::queue_modes_enabled` теперь следует отдельному intent-method `PlaylistController::queue_mode_actions_available()`: false только при отсутствующем controller или fatal invariant, но true во всех кратких install-фазах. Structural Add/Sort/Clear gate остаётся отдельным typed boundary и не протекает в persistent transport.
- Controller regression tests фиксируют TemporarilyBlocked structural availability вместе с доступными queue modes, возврат Available после Installed и обе недоступности после fatal mismatch.
