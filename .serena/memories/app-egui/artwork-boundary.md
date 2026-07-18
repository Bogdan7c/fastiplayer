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
