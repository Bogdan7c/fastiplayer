//! Переиспользуемая ручная отрисовка интерфейса rustiplayer на `egui`.
//!
//! Crate не знает о событиях, playback-состоянии и виджетах: вызывающая сторона
//! передаёт только painter, прямоугольники и типизированное визуальное состояние.

mod fullscreen_button;
mod media_kind_icon;
mod open_media_button;
mod playback_button;
mod playback_rate_button;
mod settings_button;
mod sidebar_buttons;
mod timeline;
mod transport_button;
mod video_dim_overlay;
mod volume_button;
mod volume_separator;
mod volume_slider;
mod window_controls;
mod window_title;

use egui::Painter;

pub use fullscreen_button::{FullscreenGlyph, FullscreenStyle};
pub use media_kind_icon::MediaKindGlyph;
pub use playback_button::{ButtonVisualState, PlaybackGlyph, PlaybackStyle};
pub use playback_rate_button::{PlaybackRateButtonGeometry, PlaybackRateButtonStyle};
pub use sidebar_buttons::SidebarButtonGlyph;
pub use timeline::{TimelinePaintState, TimelineStyle, timeline_track_rect};
pub use transport_button::{TransportButtonStyle, TransportGlyph};
pub use volume_button::VolumeGlyph;
pub use volume_slider::{THUMB_RADIUS as VOLUME_THUMB_RADIUS, TRACK_HEIGHT as VOLUME_TRACK_HEIGHT};
pub use window_controls::{WindowControlGlyph, WindowControlStyle};

/// Стабильный facade всей ручной отрисовки.
#[derive(Clone, Copy)]
pub struct ArtworkPainter<'a> {
    painter: &'a Painter,
}

impl<'a> ArtworkPainter<'a> {
    /// Создаёт facade поверх painter-а, принадлежащего вызывающему UI.
    #[must_use]
    pub const fn new(painter: &'a Painter) -> Self {
        Self { painter }
    }

    /// Рисует центральную кнопку воспроизведения.
    pub fn playback_button(
        self,
        rect: egui::Rect,
        glyph: PlaybackGlyph,
        state: ButtonVisualState,
        style: PlaybackStyle,
    ) {
        playback_button::paint(self.painter, rect, glyph, state, style);
    }

    /// Рисует анимированную кнопку сброса скорости с вогнутой левой гранью.
    pub fn playback_rate_button(
        self,
        geometry: PlaybackRateButtonGeometry,
        label: Option<&str>,
        state: ButtonVisualState,
        style: PlaybackRateButtonStyle,
    ) {
        playback_rate_button::paint(self.painter, geometry, label, state, style);
    }

    /// Рисует компактную кнопку перехода к предыдущему или следующему элементу.
    pub fn transport_button(
        self,
        rect: egui::Rect,
        glyph: TransportGlyph,
        state: ButtonVisualState,
        style: TransportButtonStyle,
    ) {
        transport_button::paint(self.painter, rect, glyph, state, style);
    }

    /// Рисует кнопку открытия медиа.
    pub fn open_media_button(
        self,
        rect: egui::Rect,
        state: ButtonVisualState,
        stroke: egui::Stroke,
        hover_fill: egui::Color32,
    ) {
        open_media_button::paint(self.painter, rect, state, stroke, hover_fill);
    }

    /// Рисует компактную нейтральную иконку типа медиа.
    pub fn media_kind_icon(self, rect: egui::Rect, glyph: MediaKindGlyph, stroke: egui::Stroke) {
        media_kind_icon::paint(self.painter, rect, glyph, stroke);
    }

    /// Рисует кнопку полноэкранного режима.
    pub fn fullscreen_button(
        self,
        rect: egui::Rect,
        glyph: FullscreenGlyph,
        state: ButtonVisualState,
        style: FullscreenStyle,
    ) {
        fullscreen_button::paint(self.painter, rect, glyph, state, style);
    }

    /// Рисует кнопку громкости.
    pub fn volume_button(
        self,
        rect: egui::Rect,
        glyph: VolumeGlyph,
        state: ButtonVisualState,
        stroke: egui::Stroke,
        hover_fill: egui::Color32,
    ) {
        volume_button::paint(self.painter, rect, glyph, state, stroke, hover_fill);
    }

    /// Рисует ползунок громкости.
    pub fn volume_slider(
        self,
        rect: egui::Rect,
        volume: f32,
        state: ButtonVisualState,
        active_fill: egui::Color32,
        stroke_width: f32,
    ) {
        volume_slider::paint(self.painter, rect, volume, state, active_fill, stroke_width);
    }

    /// Рисует разделитель блока громкости.
    pub fn volume_separator(self, rect: egui::Rect, color: egui::Color32) {
        volume_separator::paint(self.painter, rect, color);
    }

    /// Рисует settings-кнопку titlebar-а.
    pub fn settings_button(
        self,
        rect: egui::Rect,
        state: ButtonVisualState,
        stroke: egui::Stroke,
        hover_fill: egui::Color32,
    ) {
        settings_button::paint(self.painter, rect, state, stroke, hover_fill);
    }

    /// Рисует одну из нейтральных иконок переключателя sidebar.
    pub fn sidebar_button(
        self,
        rect: egui::Rect,
        glyph: SidebarButtonGlyph,
        state: ButtonVisualState,
        stroke: egui::Stroke,
        hover_fill: egui::Color32,
    ) {
        sidebar_buttons::paint(self.painter, rect, glyph, state, stroke, hover_fill);
    }

    /// Рисует постоянный active-фон titlebar-кнопки.
    pub fn sidebar_button_active_background(self, rect: egui::Rect, fill: egui::Color32) {
        sidebar_buttons::paint_active_background(self.painter, rect, fill);
    }

    /// Рисует timeline по уже вычисленным долям.
    pub fn timeline(self, rect: egui::Rect, state: TimelinePaintState, style: TimelineStyle) {
        timeline::paint(self.painter, rect, state, style);
    }

    /// Рисует заголовок окна внутри заданного clip rect.
    pub fn window_title(self, rect: egui::Rect, title: &str, color: egui::Color32) {
        window_title::paint(self.painter, rect, title, color);
    }

    /// Рисует системную кнопку titlebar-а.
    pub fn window_control(
        self,
        rect: egui::Rect,
        glyph: WindowControlGlyph,
        state: ButtonVisualState,
        style: WindowControlStyle,
    ) {
        window_controls::paint(self.painter, rect, glyph, state, style);
    }

    /// Затемняет видео сплошной полупрозрачной заливкой.
    pub fn video_dim_overlay(self, rect: egui::Rect, color: egui::Color32) {
        video_dim_overlay::paint(self.painter, rect, color);
    }
}

#[cfg(test)]
mod tests {
    use egui::{Color32, Context, FontId, RawInput, Rect, Shape, Stroke, Vec2, pos2};

    use super::*;

    fn painted_shape_count(mut paint: impl FnMut(ArtworkPainter<'_>)) -> usize {
        let context = Context::default();
        let output = context.run_ui(RawInput::default(), |ui| {
            paint(ArtworkPainter::new(ui.painter()));
        });
        output.shapes.len()
    }

    fn rect() -> Rect {
        Rect::from_min_size(pos2(10.0, 20.0), Vec2::splat(40.0))
    }

    fn playback_style() -> PlaybackStyle {
        PlaybackStyle {
            diameter: 36.0,
            icon_extent: 14.0,
            stroke_width: 2.0,
            color: Color32::WHITE,
            hover_fill: Color32::GRAY,
        }
    }

    fn transport_style() -> TransportButtonStyle {
        TransportButtonStyle {
            icon_extent: 18.0,
            bar_width: 2.0,
            color: Color32::WHITE,
            hover_fill: Color32::GRAY,
        }
    }

    fn playback_rate_style() -> PlaybackRateButtonStyle {
        PlaybackRateButtonStyle {
            outline: Stroke::new(1.5, Color32::WHITE),
            hover_fill: Color32::from_rgba_unmultiplied(255, 255, 255, 28),
            text_color: Color32::WHITE,
            font_id: FontId::proportional(14.0),
        }
    }

    #[test]
    fn playback_states_have_stable_shape_counts() {
        assert_eq!(
            painted_shape_count(|p| p.playback_button(
                rect(),
                PlaybackGlyph::Play,
                ButtonVisualState::Idle,
                playback_style()
            )),
            2
        );
        assert_eq!(
            painted_shape_count(|p| p.playback_button(
                rect(),
                PlaybackGlyph::Pause,
                ButtonVisualState::Hovered,
                playback_style()
            )),
            4
        );
    }

    #[test]
    fn transport_states_have_stable_shape_counts() {
        assert_eq!(
            painted_shape_count(|painter| painter.transport_button(
                rect(),
                TransportGlyph::Previous,
                ButtonVisualState::Idle,
                transport_style(),
            )),
            2
        );
        assert_eq!(
            painted_shape_count(|painter| painter.transport_button(
                rect(),
                TransportGlyph::Next,
                ButtonVisualState::Hovered,
                transport_style(),
            )),
            3
        );
    }

    #[test]
    fn playback_rate_button_states_have_stable_shape_counts() {
        // Полностью открытая geometry не обрезает ни контур, ни подпись.
        let geometry = PlaybackRateButtonGeometry {
            button_rect: Rect::from_min_size(pos2(20.0, 20.0), Vec2::new(48.0, 32.0)),
            visible_clip_rect: Rect::from_min_size(pos2(20.0, 20.0), Vec2::new(48.0, 32.0)),
            concave_radius: 24.0,
        };
        // Idle содержит только outline и текст.
        assert_eq!(
            painted_shape_count(|painter| painter.playback_rate_button(
                geometry,
                Some("1.25x"),
                ButtonVisualState::Idle,
                playback_rate_style(),
            )),
            2
        );
        // Hover добавляет один mesh той же concave-формы.
        assert_eq!(
            painted_shape_count(|painter| painter.playback_rate_button(
                geometry,
                Some("1.25x"),
                ButtonVisualState::Hovered,
                playback_rate_style(),
            )),
            3
        );
        // Нулевой reveal не оставляет невидимых shapes в paint list.
        let hidden_geometry = PlaybackRateButtonGeometry {
            visible_clip_rect: Rect::from_min_max(
                geometry.button_rect.left_top(),
                geometry.button_rect.left_bottom(),
            ),
            ..geometry
        };
        assert_eq!(
            painted_shape_count(|painter| painter.playback_rate_button(
                hidden_geometry,
                None,
                ButtonVisualState::Idle,
                playback_rate_style(),
            )),
            0
        );
    }

    #[test]
    fn playback_rate_button_left_edge_is_concave_and_stays_inside_bounds() {
        // Characterization использует production-размеры 48×28 и радиус Play/Pause 24.
        let button_rect = Rect::from_min_size(pos2(20.0, 22.0), Vec2::new(48.0, 28.0));
        // Полный clip позволяет проверять исходную geometry без animation crop.
        let geometry = PlaybackRateButtonGeometry {
            button_rect,
            visible_clip_rect: button_rect,
            concave_radius: 24.0,
        };
        // Label исключён, чтобы единственным shape оставался outline path.
        let output = Context::default().run_ui(RawInput::default(), |ui| {
            ArtworkPainter::new(ui.painter()).playback_rate_button(
                geometry,
                None,
                ButtonVisualState::Idle,
                playback_rate_style(),
            );
        });
        // Outline обязан оставаться одним детерминированным shape.
        assert_eq!(output.shapes.len(), 1);
        // Извлекаем реальные sampled points, а не дублируем формулу тестом.
        let Shape::Path(outline_path) = &output.shapes[0].shape else {
            panic!("playback-rate outline должен быть PathShape");
        };
        // Верхняя точка начинается от bounding rect.
        let top_left = outline_path.points[0];
        // Средняя точка sampled дуги должна быть правее и образовывать выемку.
        let middle_left = outline_path.points[8];
        // Нижняя точка возвращается к той же bounding X.
        let bottom_left = outline_path.points[16];
        // Обе крайние точки совпадают с прямоугольной границей.
        assert!((top_left.x - button_rect.left()).abs() < 0.0001);
        assert!((bottom_left.x - button_rect.left()).abs() < 0.0001);
        // Центр действительно вогнут внутрь кнопки.
        assert!(middle_left.x > button_rect.left());
        // Дуга симметрична относительно горизонтального центра.
        assert!((top_left.x - bottom_left.x).abs() < 0.0001);
        // Stroke целиком остаётся в ожидаемых bounds с допуском собственной толщины.
        assert!(
            button_rect
                .expand(playback_rate_style().outline.width)
                .contains_rect(outline_path.visual_bounding_rect())
        );
    }

    #[test]
    fn transport_glyphs_are_mirrored_and_stay_inside_their_hit_area() {
        // Общий hit-area задаёт ось зеркального отражения для обеих иконок.
        let hit_rect = rect();
        // Отдельный paint pass сохраняет только два shape варианта Previous.
        let previous_output = Context::default().run_ui(RawInput::default(), |ui| {
            ArtworkPainter::new(ui.painter()).transport_button(
                hit_rect,
                TransportGlyph::Previous,
                ButtonVisualState::Idle,
                transport_style(),
            );
        });
        // Второй paint pass изолирует зеркальный вариант Next.
        let next_output = Context::default().run_ui(RawInput::default(), |ui| {
            ArtworkPainter::new(ui.painter()).transport_button(
                hit_rect,
                TransportGlyph::Next,
                ButtonVisualState::Idle,
                transport_style(),
            );
        });
        // Оба варианта обязаны состоять из ограничителя и треугольника.
        assert_eq!(previous_output.shapes.len(), 2);
        assert_eq!(next_output.shapes.len(), 2);

        // Shape-ы сравниваются попарно: ограничитель с ограничителем, треугольник с треугольником.
        for (previous_shape, next_shape) in previous_output.shapes.iter().zip(&next_output.shapes) {
            // Реальные visual bounds учитывают итоговую геометрию Painter.
            let previous_bounds = previous_shape.shape.visual_bounding_rect();
            // Bounds Next должны быть точным горизонтальным отражением Previous.
            let next_bounds = next_shape.shape.visual_bounding_rect();
            // Левая граница Previous отражается в правую границу Next.
            assert!(
                (previous_bounds.left() + next_bounds.right() - hit_rect.center().x * 2.0).abs()
                    < f32::EPSILON
            );
            // Правая граница Previous отражается в левую границу Next.
            assert!(
                (previous_bounds.right() + next_bounds.left() - hit_rect.center().x * 2.0).abs()
                    < f32::EPSILON
            );
            // Вертикальная геометрия у зеркальных вариантов не меняется.
            assert_eq!(previous_bounds.top(), next_bounds.top());
            // Нижняя граница также должна полностью совпасть.
            assert_eq!(previous_bounds.bottom(), next_bounds.bottom());
            // Previous не имеет права выходить за общий hit-area.
            assert!(hit_rect.contains_rect(previous_bounds));
            // Next подчиняется тому же ограничению.
            assert!(hit_rect.contains_rect(next_bounds));
        }
    }

    #[test]
    fn media_kind_glyphs_are_visually_distinct_and_deterministic() {
        let stroke = Stroke::new(1.0, Color32::WHITE);
        let unknown_shapes = painted_shape_count(|painter| {
            painter.media_kind_icon(rect(), MediaKindGlyph::Unknown, stroke);
        });
        let audio_shapes = painted_shape_count(|painter| {
            painter.media_kind_icon(rect(), MediaKindGlyph::Audio, stroke);
        });
        let video_shapes = painted_shape_count(|painter| {
            painter.media_kind_icon(rect(), MediaKindGlyph::Video, stroke);
        });

        assert_eq!(unknown_shapes, 7);
        assert_eq!(audio_shapes, 4);
        assert_eq!(video_shapes, 2);
    }

    #[test]
    fn media_kind_glyphs_stay_inside_their_cell() {
        for glyph in [
            MediaKindGlyph::Unknown,
            MediaKindGlyph::Audio,
            MediaKindGlyph::Video,
        ] {
            // Отдельный context изолирует shape-ы каждого варианта и упрощает диагностику границ.
            let context = Context::default();
            // Общая ячейка совпадает с geometry-helper остальных artwork-тестов.
            let cell_rect = rect();
            // Реальный egui paint output позволяет проверить stroke, а не только опорные координаты.
            let output = context.run_ui(RawInput::default(), |ui| {
                ArtworkPainter::new(ui.painter()).media_kind_icon(
                    cell_rect,
                    glyph,
                    Stroke::new(1.0, Color32::WHITE),
                );
            });

            // Ни один фактически нарисованный shape не должен пересечь границу ячейки.
            assert!(output.shapes.iter().all(|clipped_shape| {
                cell_rect.contains_rect(clipped_shape.shape.visual_bounding_rect())
            }));
        }
    }

    #[test]
    fn volume_and_fullscreen_variants_remain_distinct() {
        let stroke = Stroke::new(2.0, Color32::WHITE);
        let audible = painted_shape_count(|p| {
            p.volume_button(
                rect(),
                VolumeGlyph::Audible,
                ButtonVisualState::Idle,
                stroke,
                Color32::GRAY,
            )
        });
        let muted = painted_shape_count(|p| {
            p.volume_button(
                rect(),
                VolumeGlyph::Muted,
                ButtonVisualState::Idle,
                stroke,
                Color32::GRAY,
            )
        });
        assert_eq!((audible, muted), (3, 2));
        let style = FullscreenStyle {
            icon_extent: 16.0,
            stroke,
            hover_fill: Color32::GRAY,
        };
        assert_eq!(
            painted_shape_count(|p| p.fullscreen_button(
                rect(),
                FullscreenGlyph::Enter,
                ButtonVisualState::Idle,
                style
            )),
            8
        );
        assert_eq!(
            painted_shape_count(|p| p.fullscreen_button(
                rect(),
                FullscreenGlyph::Exit,
                ButtonVisualState::Hovered,
                style
            )),
            9
        );
    }

    #[test]
    fn timeline_geometry_and_states_are_deterministic() {
        let style = TimelineStyle {
            track_height: 5.0,
            thumb_radius: 6.0,
            horizontal_padding: 8.0,
            track_fill: Color32::GRAY,
            played_fill: Color32::WHITE,
            target_fill: Color32::LIGHT_BLUE,
            thumb_fill: Color32::WHITE,
            track_outline_width: 3.0,
            track_outline_fill: Color32::BLACK,
            thumb_outline_width: 2.0,
            thumb_outline_fill: Color32::BLACK,
            disabled_fill: Color32::DARK_GRAY,
        };
        let track = timeline_track_rect(
            Rect::from_min_size(pos2(0.0, 0.0), Vec2::new(100.0, 28.0)),
            style,
        );
        assert_eq!(
            (track.left(), track.right(), track.height()),
            (8.0, 92.0, 5.0)
        );
        assert_eq!(
            painted_shape_count(|p| p.timeline(rect(), TimelinePaintState::Disabled, style)),
            3
        );
        assert_eq!(
            painted_shape_count(|p| p.timeline(
                rect(),
                TimelinePaintState::Enabled {
                    current_fraction: 0.25,
                    display_fraction: 0.75,
                    target: true
                },
                style
            )),
            6
        );
    }

    #[test]
    fn every_window_control_and_hover_state_paints() {
        let style = WindowControlStyle {
            fill: Color32::BLACK,
            stroke: Stroke::new(1.0, Color32::WHITE),
            hover_fill: Color32::GRAY,
        };
        for glyph in [
            WindowControlGlyph::Minimize,
            WindowControlGlyph::Maximize,
            WindowControlGlyph::Restore,
            WindowControlGlyph::Close,
        ] {
            assert!(
                painted_shape_count(|p| p.window_control(
                    rect(),
                    glyph,
                    ButtonVisualState::Idle,
                    style
                )) > 0
            );
            assert!(
                painted_shape_count(|p| p.window_control(
                    rect(),
                    glyph,
                    ButtonVisualState::Hovered,
                    style
                )) > 1
            );
        }
    }
}
