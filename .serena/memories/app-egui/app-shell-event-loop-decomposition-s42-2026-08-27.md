# S42 — decomposition app shell / winit event loop (2026-08-27)

## Граница владения

- Behavior-neutral split не добавил public/crate-wide API и не изменил lifecycle state. `crates/app-egui/src/app_shell/mod.rs` (712 строк) по-прежнему владеет `AppShell`, всеми process-/renderer-bound полями, constructor-ом, restore/suspend intents, timeline-seek settlement, checkpoint capture, terminal owner shutdown и последним `AppInstanceLease`.
- Приватный `crates/app-egui/src/app_shell/event_loop.rs` (310 строк) владеет только `ApplicationHandler<AppWakeEvent>` и winit policy: user-wake drain/redraw gate, window/input/redraw callbacks, resume window creation и defensive background polling. Child вызывает intent-методы `AppShell` и не хранит второго lifecycle/resource owner-а.
- Существующие private owners `hotkeys.rs` (203) и `shutdown.rs` (267) не менялись. Все production app-shell modules теперь <= 800 строк.

## Инварианты

- Suspend остаётся в порядке: sidebar resize flush -> pending same-item/media/seek settlement -> checkpoint -> local-job transfer -> binding detach -> `AppState`/renderer drop.
- Resume сохраняет existing-window reuse, renderer/AppState recreation, exact playlist binding/desktop publication и visible-mutation redraw gate.
- Terminal exit по-прежнему закрывает process owners под одним absolute deadline, drain-ит persistence/resume checkpoint и освобождает lease только после terminal owner outcomes; timeout остаётся немедленным non-zero process exit.
- Hotkey и transport dispatch, player-timeline wake, persistence wake, background polling и `ControlFlow::Wait` semantics перенесены дословно по policy, без слияния typed outcomes или positional bool API.

## Тесты и проверки

- App-shell focused tests перенесены в `crates/app-egui/src/app_shell/tests.rs` (79 строк); private hotkey/shutdown tests остаются у владельцев.
- Функциональные lifecycle regression paths остаются в `playlist_runtime/suspend_resume/tests.rs` и `playlist_runtime/resume_persistence/tests/aud014_shutdown_checkpoint.rs`: 12/12 suspend/resume и 3/3 AUD-014 seek-settlement/shutdown PASS.
- PASS: app-shell focused 13/13; full `app-egui` no-default 1002/1002 и all-features 1002/1002; strict Clippy обеих matrices/all-targets; rustfmt; diff check; refactor guardrails; S42 final acceptance 24/24; Serena diagnostics.
- `scripts/check_s42_guardrails.py` ожидаемо остаётся red: `scripts/module-size-baseline.json` всё ещё содержит запрещённую к правке legacy запись app-shell `1004`, теперь stale при 712, вместе с другими repository-wide wave deltas. Snapshot baseline в этой задаче намеренно не менялся.

## Клавиатура: владелец фокуса и Esc (UX edge cases, сессия 01, 2026-10-04)

- Хоткеи больше не гасятся флагом «egui занят» (`egui_wants_keyboard_input()` и `egui_response.consumed` = фокус у ЛЮБОГО виджета, из-за этого хоткеи глохли после клика по кнопке). `ui/keyboard_focus.rs::keyboard_focus_owner(&Context) -> KeyboardFocusOwner { Nobody, Widget, TextInput }` читает память egui предыдущего кадра; event loop передаёт результат в `hotkeys::classify_key_event`.
- Политика `hotkeys.rs`: media keys работают всегда. При `TextInput` больше ничего. При `Widget` (кнопка или строка плейлиста в фокусе) только F/M/J/L/P/N (`hotkey_coexists_with_focused_widget`): Space/Enter нажимают кнопку, стрелки двигают фокус или строки, Esc снимает фокус, PageUp/PageDown прокручивают. При `Nobody` вся таблица. Инвариант: пробел на кнопке play даёт ровно одно переключение (кнопка ИЛИ хоткей).
- Кнопки play/prev/next/сброс скорости после клика мышью делают `surrender_focus` (`clicked() && interact_pointer_pos().is_some()`, как shuffle/repeat). Tab-фокус сохраняется: на нём держатся доступность и клавиши скорости `+ / - / 0` (`playback_rate::collect_input_actions`). Решение владельца: «кликнул по play, убрал мышь, крутишь скорость» больше не работает.
- Esc → `ShellHotkeyAction::DismissTopmost` (бывший `Close`). Приложение по Esc НЕ закрывается. `app_shell/escape_dismissal.rs` работает в три шага: `EscapeContext::collect` (`egui::Popup::is_any_open`, `pending_playlist_confirmation`, `pending_playlist_import_preview`, `window.fullscreen()`) → чистая `resolve_escape_target` → `dismiss_topmost`. Приоритет: popup egui (его закрывает сам egui в кадре) → подтверждение замены очереди → preview импорта → выход из фуллскрина → ничего. Отмена идёт теми же путями, что у кнопок «Отмена» (`apply_playlist_confirmation_action`, `PlaylistAction::CancelImport`). Настройки по Esc не закрываются (решение владельца). Редактор URL отменяется по Esc только когда его поле в фокусе (собственный обработчик toolbar).
- Тесты: `app_shell/hotkeys.rs` (сквозные на настоящей кнопке через `#[cfg(test)] ui::player_controls::playback_button_test_harness`), `app_shell/escape_dismissal/tests.rs` (реальный `PlaylistRuntime` и ComboBox), `ui/keyboard_focus.rs`, фокус после клика у next (`transport.rs`), сброса скорости (`playback_rate.rs`) и таймлайна (`timeline/render.rs`).

Связанные знания: `mem:app-egui/wake-runtime-s10a`, `mem:app-egui/suspend-resume-checkpoint-s14b`, `mem:app-egui/timeline-seek-lifecycle-settlement-aud014-2026-08-23`, `mem:app-egui/playlist-persistence-s14`.