# Переиспользуемые UI-области и сохранение UX

Эти правила применяются к любым будущим сущностям внутри sidebar, inspector, drawer, dock, overlay, bottom sheet, tool panel и других составных UI-областей — не только к текущим Playlist/Settings/URL/Info.

## Host и сущность

- Сначала определить, является новая вещь новой **областью** окна или новой **сущностью внутри существующей области**. Если меняется только назначение/контент при той же геометрии и UX, это новая сущность существующего host, а не новый Panel/Window/Area.
- Host единолично владеет геометрией и UX-инвариантами: egui container, stable host ID, rect, live width/height, resize state, persisted size policy, min/max range, open/close, displacement video viewport, clipping, z-order, animation и repaint.
- Toolkit remembered-state не должен становиться вторым владельцем размера. Если container внутренне persist-ит geometry, host обязан нейтрализовать/синхронизировать её так, чтобы authoritative live state оставался один.
- Сущность владеет только своим domain/UI state, read-only model/snapshot, content-specific scroll/focus IDs и typed actions. Сущности запрещено создавать container того же уровня, хранить копию host width/open state или менять viewport displacement.
- Добавление сущности должно расширять typed section/entity enum и единый content renderer/registry. Нельзя добавлять новый `Panel::left`, `Window`, `Area` или section-derived host ID ради нового содержимого.

## Stable ID и размер

- У одной визуальной области ровно один stable host ID на всех сущностях и animation phases. Dynamic ID из title/section создаёт независимое persisted state и считается архитектурной ошибкой.
- Общий пользовательский размер сохраняется при переключении сущностей, close/reopen и process restart. Min/max/default policy задаётся один раз владельцем config/host и одинакова для всех сущностей, если отдельное UX-решение явно не согласовано.
- Live resize передаётся наружу только typed output/event boundary. Внешний код не читает внутренние поля host; animation/intermediate clipped size не считается пользовательским resize. Geometry читается из собственного rect container-а до render content, а не из content-dependent response rect: translated/clipped children могут расширять или сжимать response во время transition. После render host обязан сохранить свой rect как occupied area parent layout, чтобы toolkit cursor и persisted container state не зависели от видимого содержимого.
- Persisted resize обязан иметь явные debounce, wake scheduling, forced lifecycle flush и rollback-to-committed при persistence failure.
- Контент не должен определять размер host. Длинные строки, metadata, списки и ошибки обязаны wrap/scroll/clip внутри available rect.
- Fixed `UiBuilder::max_rect` допустим для clipped animation copies. В fully-open resizable состоянии контент рендерится прямо в host UI; fixed child rect там может превратить текущий размер в content minimum и заблокировать resize handle.

## Lifecycle и input

- Переключение сущности не должно неявно уничтожать её draft, selection, validation, preview или domain lifecycle. Hide, Cancel, Apply, OK и fatal error остаются разными typed outcomes.
- Outgoing/incoming animation copies получают разные stable content/ScrollArea IDs, общий clip rect и не принимают input до завершения перехода.
- Активная open/close/content animation явно запрашивает repaint, включая paused playback.
- Host-level close скрывает область. Если конкретная сущность требует rollback (например Settings Cancel), это отдельный explicit entity action, а не общая семантика host close.

## UX parity для расширений

- Новая сущность обязана наследовать существующие: положение, размер и resize handle; способ вытеснения/перекрытия соседнего контента; скорость/easing анимации; close/reopen; titlebar hit-testing; одновременную видимость независимых областей; remembered size.
- Нельзя исправлять layout сущности изменением host geometry, если причина находится в content layout.
- Если новая сущность действительно требует другой геометрии, поведения resize или lifecycle области, это важное архитектурное/UX-решение: остановиться и согласовать создание отдельного host с пользователем.

## Обязательные regression tests

- Guardrail: у переиспользуемой области ровно один site создания host container и отсутствуют entity-specific host IDs.
- Переключение каждой пары сущностей сохраняет тот же host rect/remembered size и не создаёт второй container.
- Реальный headless resize работает после открытия каждой сущности и после нескольких переключений; animation size не публикует resize event.
- Persist debounce coalesces latest value, same rounded value не пишет повторно, failure восстанавливает committed host size, suspend/shutdown force-flush pending.
- Длинный content не увеличивает host size; применяется wrap/scroll/clip.
- Open/close/content transition сохраняют direction, duration, clip, repaint и input exclusion.
- Проверяются разные lifecycle semantics сущностей: hide сохраняет state, explicit Cancel откатывает, Apply/OK/error не смешиваются.
- Изменение host architecture требует обновить эту memory и focused UI/layout guardrails.

Текущая реализация и конкретные sidebar-инварианты: `mem:app-egui/sidebar-controller`.

## Видимость chrome в фуллскрине (UX 14, 2026-10-08)

- Единственный владелец видимости titlebar/нижней панели/курсора — `crate::fullscreen_chrome::FullscreenChromeController` (поле `AppState`). Панели НЕ знают об автоскрытии: их сдвигает генерик `ui::edge_slide` (дочерний `Ui` + «распорка»-Panel на видимую часть), обёртки — `ui::fullscreen_chrome_panels`. Новые элементы chrome, которые должны прятаться вместе с панелями, подключаются через `edge_slide`, а не собственным таймером; новая причина «не прятать» — новый вариант `ChromeHoldReason`.
- Инвариант: при `hidden_fraction == 0` (всегда вне фуллскрина) раскладка окна идентична прямым панелям — закреплено тестом `edge_slide::tests`. Детали: `mem:app-egui/fullscreen-chrome-autohide-ux14`. Painter boundary: `mem:app-egui/artwork-boundary`.

## Глобальное поведение egui (с 2026-10-04; сейчас egui 0.36.2)

- egui 0.36 (сессия 02 обновления): `clip_rect_margin` удалён из `apply_app_egui_behavior` (в 0.36 поле deprecated и ни на что не влияет; владелец принял отличие — обводки у краёв ScrollArea могут срезаться на ≤3pt, `content_margin` НЕ добавлять). Добавлена нейтрализация `options.sync_window_theme = false` (0.36 иначе шлёт окну `ViewportCommand::SetTheme` и просит лишний repaint); тест `egui_does_not_push_theme_to_native_window`, проверен мутацией. Строки ниже про clip-запас — история 0.35.
- Separator панелей egui 0.36 (#8367): egui резервирует ~1pt под линию внутри размера панели (`default_size`/`size_range` — внешний размер). Владелец принял вид 0.36 (содержимое панелей на 1pt уже, линия внутри панели). Sidebar берёт ширину хоста и границу видео из внешнего `PanelState::outer_rect` (`sidebar::sidebar_host_outer_rect`), иначе сохранённая ширина уползала бы на 1pt за кадр; геометрические тесты 420/500/250 и `remaining_rect.left()==500` это закрепляют. На первом кадре выезда (ширина 1 pt) весь pt занимает линия и область содержимого пустая — инвариант `is_positive` проверяется только для внешнего host rect (тест `first_opening_frame_with_minimal_width_keeps_one_point_host`). Debug-assert-ы ловятся только debug-сборкой: smoke/frame timing идут в release.
- egui 0.36 в debug паникует при drop `TexturesDelta` с неприменёнными дельтами. Headless-тесты гоняют кадры через `#[cfg(test)] ui::test_frame::run_ui_frame` (и копию в `ui-artwork-egui::test_frame`), а тесты про repaint — через `test_frame::app_behavior_context()`; прямой `ctx.run_ui` в тестах не использовать.

- Единственный владелец глобальных настроек поведения egui — `crates/app-egui/src/ui/egui_behavior.rs`. `apply_app_egui_behavior(&Context)` вызывается один раз в `AppState::new` после `set_theme` и через `all_styles_mut` фиксирует прежнее (egui 0.34) поведение: `animation_time = 6/60 с`, `visuals.clip_rect_margin = 3.0`, `visuals.ime_composition.legacy_visuals = true`. Решение владельца: поведение UI при обновлениях egui не меняется; новые дефолты egui нейтрализуются здесь, а не по месту виджетов. Цвета/размеры controls по-прежнему в `ui::skin`.
- Все вертикальные `ScrollArea` приложения создаются только через `egui_behavior::vertical_scroll_area()` (= `ScrollSource::ALL`, прокрутка перетаскиванием мышью как в 0.34; в 0.35 по умолчанию только touch). Guard-тест запрещает прямой `ScrollArea::vertical(` вне модуля и ожидает ровно 6 использований (settings layout ×2, URL, sidebar Info, playlist, telemetry) — при добавлении новой ScrollArea обновить число.
- Функциональные тесты в модуле (анимация 0.1 с, clip-запас 3pt, mouse drag-scroll, IME без подчёркиваний) проверены мутацией: при дефолтах egui 0.35 падают. Panel-ы используют `show` (`show_inside` deprecated в 0.35); `show_collapsible`/`show_switched` (слайд, drag-to-close, double-click toggle) не используются — у сайдбара собственная анимация.

## S24 concrete URL entity (2026-07-22)

- URL stream configuration подтверждает host/entity boundary: новый `ui/url_sidebar.rs` является только content renderer-ом `SidebarSection::Url`; единственный `egui::Panel::left(app_sidebar)` остаётся в `ui/sidebar.rs`.
- Entity получает immutable secret-safe `UrlSidebarModel`, не владеет width/open/animation/viewport и не имеет URL input либо queue mutation. Local/direct/YtDlp являются typed content states одного host-а, а не отдельными panels.
- Regression guardrail отдельно проверяет отсутствие `Panel::` в URL content module; общий sidebar suite проверяет единственный shared constructor и сохранение geometry/resize behavior.