# Artwork boundary app-egui

- `ui-artwork-egui` — переиспользуемый workspace-crate ручной векторной отрисовки на egui; его единственная normal dependency — `egui`.
- Стабильный facade: `ui_artwork_egui::ArtworkPainter`, принимающий только `&egui::Painter`. Crate не принимает `Ui`, `Response`, `PlayerSkin`, `PlayerSnapshot` или media/player типы.
- `app-egui` владеет hit-area, accessibility/widget info, hover/drag detection, командами и преобразованием доменного состояния в `PlaybackGlyph`, `VolumeGlyph`, `FullscreenGlyph`, `WindowControlGlyph`, `ButtonVisualState`, `TimelinePaintState`.
- Вся новая custom-рисовка должна появляться отдельным `.rs` в `crates/ui-artwork-egui/src/`; прямые Painter-примитивы в `crates/app-egui/src` запрещает `scripts/check-refactor-guardrails.py`.
- Timeline track geometry принадлежит artwork crate: `timeline_track_rect` используется и paint path, и app-side pointer mapping, чтобы hit-testing совпадал с изображением.
- Characterization-тесты фигур и геометрии находятся в `crates/ui-artwork-egui/src/lib.rs`; action/accessibility/hit-area regression tests остаются в `app-egui`.
- Previous/Next transport artwork использует доменно-нейтральный `TransportGlyph::{Previous, Next}`, `TransportButtonStyle` и `ArtworkPainter::transport_button(...)`. `ui-artwork-egui` владеет зеркальной геометрией ограничителя/треугольника и hover-подложкой; `app-egui` владеет anchored hit-area `32x32`, расстоянием центров `64` от play/pause через `ControlsStyle`, availability/disabled color, accessibility/focus и typed transport actions.
- Playlist media-kind artwork использует доменно-нейтральный `MediaKindGlyph::{Unknown, Audio, Video}` и `ArtworkPainter::media_kind_icon(rect, glyph, stroke)`. `app-egui` владеет переводом `PlaylistMediaKind` в glyph, фиксированной ячейкой строки и accessibility-текстом; artwork-crate владеет геометрией ноты, видеокадра Play и неизвестного файла.
- Titlebar title, settings gear, window controls и video dim overlay также проходят через facade; прежние места вызова остаются владельцами layout/runtime semantics.


## Playback-rate reset artwork (2026-07-18)

- Анимированная кнопка сброса скорости проходит через `ArtworkPainter::playback_rate_button(...)`; geometry/style types и concave Painter primitives живут в `crates/ui-artwork-egui/src/playback_rate_button.rs`.
- `app-egui::ui::player_controls::playback_rate::indicator` владеет stable egui animation Id, 250-ms cubic transition, layout/hit-area/accessibility и typed `ResetPlaybackRate`; `ui-artwork-egui` не знает о `PlaybackRate`, `PlayerSnapshot` или командах.
- Fully-open preferred geometry MinimalSkin: 48x28 points (2-point vertical inset сверху и снизу относительно Next), bounding gap 5 points от Play/Pause, прозрачный светлый outline и общий translucent hover fill. Next сдвигается на тот же eased resolved width; narrow layout уменьшает обе величины до безопасного остатка перед Fullscreen.
- При snapshot `1x` label и reset interaction отсутствуют; во время closing рисуется только пустой контур, поэтому надпись `1x` не появляется.
- Characterization-тесты concave path/hover mesh находятся в `ui-artwork-egui/src/lib.rs`; layout, 250-ms timing/reversal, click/accessibility и narrow non-overlap tests — в app-egui playback-rate/transport tests.


## Persistent queue-mode controls (2026-07-18)

- Нейтральная векторная рисовка Shuffle/Repeat/Repeat One живёт в `ui-artwork-egui::queue_mode_controls` и принимает только `QueueModeGlyph`, `QueueModePaintState` и `QueueModeControlStyle`.
- `ui-artwork-egui` не зависит от playlist/player типов и не владеет layout, hit-testing, accessibility, animation или actions.
- `app-egui::ui::player_controls::queue_mode_controls` переводит authoritative playlist snapshot в нейтральный glyph/paint-state, владеет interaction, accessibility и stable egui IDs.
- `PersistentControlStyle` хранится в app skin и передаёт artwork все foreground/surface/focus токены; конкретные queue-mode кнопки не хардкодят цвета.
- Characterization-тесты artwork закрепляют bounds, число/тип фигур, дополнительную геометрическую цифру `1` и active surface без изменения glyph geometry.

## Custom titlebar edge alignment (2026-07-18)
- `app-egui::ui::skin::ControlsStyle` владеет общей горизонтальной сеткой левых window controls и осью крайних bottom controls: offset от внутреннего content-edge равен половине `playback_button_diameter` плюс `playback_button_vertical_raise`, panel margin превращает его в первый inset от края всего окна, а `left_edge_control_center_step` задаёт общий шаг titlebar/playlist toolbar.
- Чистая геометрия custom titlebar живёт в `crates/app-egui/src/ui/window_chrome/geometry.rs`; interaction/actions остаются в `window_chrome.rs`, artwork glyph geometry не меняется.
- Центр первой левой titlebar-кнопки совпадает с центром Open, вся левая группа сохраняет button size/gap; центр Close совпадает с Fullscreen, а Minimize/Maximize/Close остаются contiguous группой вместе с hover/hit rects.
- Reserved/drag/resize geometry использует те же вычисленные оси. Resize guard покрывает фактические button-group rects, но не забирает свободные window corners.
- Title rect симметрично резервирует большую из боковых occupied widths и остаётся центрированным относительно всего окна.


## Playlist row surfaces and separator (2026-07-18)
- `ArtworkPainter` adds `reserve_playlist_row_background`, `playlist_row_background` and `playlist_row_separator`; implementation lives in the dedicated `ui-artwork-egui/src/playlist_row.rs` module and remains domain/UI-state neutral.
- The app reserves a painter shape slot before content, obtains the single full-row response after layout, then fills that earlier slot so hover/selection stays behind text. Separator is painted last over the row using a pixel-aligned `1 / pixels_per_point` stroke spanning the exact row width; row height is unchanged.
- `PlaylistRowStyle` remains app skin vocabulary and supplies fill/stroke/color tokens to the neutral facade. Artwork owns only geometry and shape ordering, never selection/playback semantics, hit-testing or commands.
- Characterization tests cover one line, full width, exactly one physical pixel and alignment at 1.0/1.25/1.5/2.0/2.5 scale factors. `ui-artwork-egui` has 15 passing tests after this boundary change.


## Playlist icon-only toolbar (2026-07-18)
- `ui-artwork-egui::playlist_toolbar` owns the five neutral hand-drawn vector glyphs `AddFiles`, `AddUrl`, `Sort`, `CurrentItem`, and `Clear`, plus the surface/focus geometry behind `ArtworkPainter::playlist_toolbar_button(...)`.
- `app-egui::ui::playlist::toolbar::icon_bar` owns the full-width row layout, stable hit areas/IDs, pointer and keyboard interaction, accessibility/tooltips, disabled reasons, sort popup, and mapping to existing `PlaylistAction`; it contains no queue/domain mutation.
- `PlayerSkin::playlist_toolbar_style()` владеет button size, optical Y offset, отдельными left/Clear padding, icon extent/stroke, grayscale foreground states, hover/pressed surfaces и focus outline. Общая skin-owned сетка левых window controls задаётся через `ControlsStyle`: Minimal skin использует центры 39/79/119/159 points от левого края (первый inset 39, шаг 40). Toolbar сохраняет 32x32-point hit areas, поэтому между ними остаётся 8-point gap; left group padding равен 23 points. Clear не входит в эту сетку и сохраняет прежний независимый right padding 18 points. Glyph остаётся 23.5 points, stroke 1.6, optical Y offset 8, enabled foreground совпадает с bottom transport (`gray 230`).
- The app still contains no direct Painter primitives. Artwork characterization tests live with the dedicated module and cover shape counts, distinct geometry, bounds, and decoration invariance; app headless tests cover layout at 350/420/600 points, exact actions, disabled controls, sort popup, Tab+Space/Enter, and Russian accessibility labels.

## Animated Playlist Undo artwork (2026-07-18)
- `ui-artwork-egui::playlist_toolbar` добавляет нейтральный `PlaylistToolbarGlyph::Undo`: открытая против часовой стрелки дуга с round caps и выраженным наконечником. Artwork владеет только векторной геометрией.
- `PlaylistToolbarPaintState` принимает нейтральные `opacity` и `content_scale`; scale применяется вокруг неизменного центра hit-area, opacity — ко всем цветам. Некорректные значения безопасно нормализуются.
- `app-egui` остаётся owner layout, stable ID, hit-testing, tooltip, AccessKit, visibility animation и typed intent. На fade-out app рисует остаточный glyph напрямую через artwork без `Response`/accessibility node.
- Characterization tests закрепляют число фигур/bounds, отличие Undo от Sort/Current/Clear, неизменный центр при opacity/scale и fractional HiDPI.
