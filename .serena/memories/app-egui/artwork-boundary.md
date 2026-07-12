# Artwork boundary app-egui

- `ui-artwork-egui` — переиспользуемый workspace-crate ручной векторной отрисовки на egui; его единственная normal dependency — `egui`.
- Стабильный facade: `ui_artwork_egui::ArtworkPainter`, принимающий только `&egui::Painter`. Crate не принимает `Ui`, `Response`, `PlayerSkin`, `PlayerSnapshot` или media/player типы.
- `app-egui` владеет hit-area, accessibility/widget info, hover/drag detection, командами и преобразованием доменного состояния в `PlaybackGlyph`, `VolumeGlyph`, `FullscreenGlyph`, `WindowControlGlyph`, `ButtonVisualState`, `TimelinePaintState`.
- Вся новая custom-рисовка должна появляться отдельным `.rs` в `crates/ui-artwork-egui/src/`; прямые Painter-примитивы в `crates/app-egui/src` запрещает `scripts/check-refactor-guardrails.py`.
- Timeline track geometry принадлежит artwork crate: `timeline_track_rect` используется и paint path, и app-side pointer mapping, чтобы hit-testing совпадал с изображением.
- Characterization-тесты фигур и геометрии находятся в `crates/ui-artwork-egui/src/lib.rs`; action/accessibility/hit-area regression tests остаются в `app-egui`.
- Titlebar title, settings gear, window controls и video dim overlay также проходят через facade; прежние места вызова остаются владельцами layout/runtime semantics.
