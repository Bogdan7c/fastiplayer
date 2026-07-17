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
