//! Skin boundary для controls/timeline.
//!
//! `AppState` выбирает skin по `ui.skin`, а конкретные виджеты используют только
//! этот контракт. Так будущие SVG, texture assets и анимации не потребуют менять
//! command/snapshot wiring.

pub mod minimal;

use egui::{Color32, Frame, Margin, Stroke};

use crate::ui::animation::AnimationState;
use crate::ui::assets::AssetProvider;

pub use minimal::MinimalSkin;

/// Единственный skin id, который текущая schema version поддерживает.
pub const MINIMAL_SKIN_ID: &str = "minimal";

/// Typed skin id после validation config-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinId {
    /// Минимальный player-style skin.
    Minimal,
}

impl SkinId {
    /// Парсит skin id из config.
    #[must_use]
    pub fn parse(raw_skin_id: &str) -> Option<Self> {
        match raw_skin_id.trim() {
            MINIMAL_SKIN_ID => Some(Self::Minimal),
            _ => None,
        }
    }
}

/// Цвета и размеры timeline.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TimelineStyle {
    /// Высота интерактивной области timeline.
    pub hit_height: f32,

    /// Толщина фоновой дорожки.
    pub track_height: f32,

    /// Радиус бегунка.
    pub thumb_radius: f32,

    /// Горизонтальный отступ дорожки внутри hit area.
    pub horizontal_padding: f32,

    /// Цвет фона timeline.
    pub track_fill: Color32,

    /// Цвет пройденной части timeline.
    pub played_fill: Color32,

    /// Цвет target-позиции во время scrub.
    pub target_fill: Color32,

    /// Цвет бегунка.
    pub thumb_fill: Color32,

    /// Толщина тёмной визуальной обводки дорожки без расширения hit area.
    pub track_outline_width: f32,

    /// Цвет тёмной визуальной обводки дорожки.
    pub track_outline_fill: Color32,

    /// Толщина тёмной визуальной обводки бегунка.
    pub thumb_outline_width: f32,

    /// Цвет тёмной визуальной обводки бегунка.
    pub thumb_outline_fill: Color32,

    /// Цвет disabled timeline.
    pub disabled_fill: Color32,
}

/// Общие цветовые токены постоянных toggle-controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PersistentControlStyle {
    /// Цвет неактивного glyph без hover.
    pub foreground_idle: Color32,
    /// Цвет неактивного glyph при hover.
    pub foreground_hover: Color32,
    /// Цвет подтверждённого активного glyph.
    pub foreground_active: Color32,
    /// Цвет glyph при запрещённом interaction.
    pub foreground_disabled: Color32,
    /// Полностью прозрачная поверхность неактивной кнопки.
    pub surface_idle: Color32,
    /// Круглая поверхность при hover неактивной кнопки.
    pub surface_hover: Color32,
    /// Постоянная слабая поверхность подтверждённой активной кнопки.
    pub surface_active: Color32,
    /// Поверхность активной кнопки при hover.
    pub surface_active_hover: Color32,
    /// Поверхность во время pointer/key press.
    pub surface_pressed: Color32,
    /// Контур keyboard focus.
    pub focus_outline: Color32,
}

/// Цветовые токены Playlist row не зависят от системного selection theme.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaylistRowStyle {
    /// Hover обычной строки.
    pub hover_fill: Color32,
    /// Постоянная selection surface.
    pub selected_fill: Color32,
    /// Усиленная selection surface под pointer.
    pub selected_hover_fill: Color32,
    /// Независимая surface active playback.
    pub active_fill: Color32,
    /// Нейтральная геометрия и цвет левого active playback marker-а.
    pub active_marker: ui_artwork_egui::PlaylistRowMarkerStyle,
    /// Контрастный цвет заголовка подтверждённо активной строки.
    pub active_title_color: Color32,
    /// Full-width physical-pixel separator.
    pub separator_color: Color32,
    /// Контур insertion target во время drag.
    pub insertion_stroke: Stroke,
    /// Контур keyboard interaction cursor.
    pub focus_stroke: Stroke,
}

/// Skin-owned геометрия и цвета Undo в заголовке Playlist.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaylistHeaderUndoStyle {
    /// Сторона стабильной квадратной hit-area.
    pub hit_area_size: f32,
    /// Text style, с высотой которого совпадает итоговый видимый glyph.
    pub glyph_text_style: egui::TextStyle,
    /// Усиленный визуальный вес дуги и открытого наконечника.
    pub glyph_stroke_width: f32,
    /// Радиус hover/pressed surface.
    pub surface_corner_radius: f32,
    /// Цвет glyph без взаимодействия.
    pub foreground_idle: Color32,
    /// Цвет glyph под pointer и во время нажатия.
    pub foreground_hover: Color32,
    /// Цвет glyph при запрещённом interaction.
    pub foreground_disabled: Color32,
    /// Временная поверхность под pointer.
    pub surface_hover: Color32,
    /// Усиленная поверхность во время pointer/key press.
    pub surface_pressed: Color32,
    /// Контур keyboard focus.
    pub focus_outline: Stroke,
    /// Отступ focus outline внутрь hit-area.
    pub focus_inset: f32,
}

/// Skin-owned геометрия и цвета компактного toolbar плейлиста.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaylistToolbarStyle {
    /// Сторона квадратной hit-area каждой кнопки.
    pub button_size: f32,
    /// Интервал между четырьмя обычными действиями слева.
    pub button_gap: f32,
    /// Отступ левой группы от края sidebar.
    pub left_group_padding: f32,
    /// Отступ отдельной Clear-кнопки от правого края sidebar.
    pub clear_right_padding: f32,
    /// Полный размер glyph внутри hit-area.
    pub icon_extent: f32,
    /// Единый визуальный вес открытых линий.
    pub glyph_stroke_width: f32,
    /// Радиус мягкой hover/pressed-подложки.
    pub surface_corner_radius: f32,
    /// Цвет glyph без взаимодействия.
    pub foreground_idle: Color32,
    /// Цвет glyph под pointer и во время нажатия.
    pub foreground_hover: Color32,
    /// Цвет glyph при запрещённом действии.
    pub foreground_disabled: Color32,
    /// Поверхность под pointer.
    pub surface_hover: Color32,
    /// Усиленная поверхность во время pointer/key press.
    pub surface_pressed: Color32,
    /// Контур keyboard focus.
    pub focus_outline: Stroke,
    /// Отступ focus outline внутрь hit-area.
    pub focus_inset: f32,
}

/// Цвета и размеры панели controls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ControlsStyle {
    /// Фон верхней панели.
    pub top_panel_fill: Color32,

    /// Фон нижней панели.
    pub bottom_panel_fill: Color32,

    /// Цвет текста normal controls.
    pub text_color: Color32,

    /// Ширина компактной кнопки.
    pub button_width: f32,

    /// Высота компактной кнопки.
    pub button_height: f32,

    /// Диаметр центральной круглой play/pause-кнопки.
    pub playback_button_diameter: f32,

    /// Размер glyph внутри центральной play/pause-кнопки.
    pub playback_button_icon_extent: f32,

    /// Толщина обводки центральной play/pause-кнопки.
    pub playback_button_stroke_width: f32,

    /// Смещение центральной play/pause-кнопки вверх относительно строки controls.
    pub playback_button_vertical_raise: f32,

    /// Шаг общих левых осей titlebar и playlist toolbar.
    pub left_edge_control_center_step: f32,

    /// Заливка центральной play/pause-кнопки при hover.
    pub playback_button_hover_fill: Color32,

    /// Размер квадратной hit-area кнопок Previous и Next.
    pub transport_button_size: f32,

    /// Расстояние от центра play/pause до центра каждой transport-кнопки.
    pub transport_button_center_distance: f32,

    /// Полный размер glyph внутри transport-кнопки.
    pub transport_button_icon_extent: f32,

    /// Ширина вертикального ограничителя transport glyph.
    pub transport_button_bar_width: f32,

    /// Цвет disabled transport glyph.
    pub transport_button_disabled_color: Color32,

    /// Заливка transport-кнопки при hover.
    pub transport_button_hover_fill: Color32,

    /// Общие цвета постоянных Shuffle/Repeat toggle-controls.
    pub persistent_control: PersistentControlStyle,

    /// Предпочтительное расстояние Shuffle/Repeat от центра Play/Pause.
    pub queue_mode_button_center_distance: f32,

    /// Минимальный зазор Next–Repeat и внешних controls до статических соседей.
    pub queue_mode_neighbor_gap: f32,

    /// Толщина открытых линий Shuffle/Repeat glyph.
    pub queue_mode_glyph_stroke_width: f32,

    /// Толщина видимого keyboard-focus outline.
    pub queue_mode_focus_outline_width: f32,

    /// Отступ focus outline внутрь 32-point hit-area.
    pub queue_mode_focus_inset: f32,

    /// Предпочтительная ширина раскрытой кнопки сброса скорости.
    pub playback_rate_button_width: f32,

    /// Отступ между bounding rect Play/Pause и кнопкой сброса скорости.
    pub playback_rate_button_gap: f32,

    /// Вертикальный inset кнопки скорости относительно 32-point Next hit-area.
    pub playback_rate_button_vertical_inset: f32,

    /// Толщина светлого контура кнопки сброса скорости.
    pub playback_rate_button_stroke_width: f32,

    /// Размер квадратной fullscreen-кнопки в нижней панели.
    pub fullscreen_button_size: f32,

    /// Размер hand-drawn fullscreen glyph внутри кнопки.
    pub fullscreen_icon_extent: f32,

    /// Ширина volume slider.
    pub volume_slider_width: f32,

    /// Внутренний отступ панели.
    pub panel_margin: Margin,
}

impl ControlsStyle {
    /// Возвращает расстояние от внутреннего края нижней панели до общей оси
    /// центров крайних кнопок Open и Fullscreen.
    ///
    /// Строка controls имеет высоту `playback_button_diameter`, а все кнопки
    /// подняты на `playback_button_vertical_raise`. Поэтому эта формула
    /// одновременно сохраняет зеркальные боковой и нижний отступы.
    #[must_use]
    pub(crate) fn bottom_edge_button_center_offset_points(self) -> f32 {
        self.playback_button_diameter * 0.5 + self.playback_button_vertical_raise
    }

    /// Возвращает inset первой общей левой оси относительно всего окна.
    #[must_use]
    pub(crate) fn left_edge_control_first_center_inset_points(self) -> f32 {
        self.panel_margin.leftf() + self.bottom_edge_button_center_offset_points()
    }
}

/// Общий контракт skin-а.
pub trait PlayerSkin: AssetProvider {
    /// Возвращает typed id skin-а.
    #[must_use]
    fn id(&self) -> SkinId;

    /// Возвращает стиль timeline.
    #[must_use]
    fn timeline_style(&self) -> TimelineStyle;

    /// Возвращает стиль controls.
    #[must_use]
    fn controls_style(&self) -> ControlsStyle;

    /// Возвращает explicit Playlist row tokens без системного синего.
    #[must_use]
    fn playlist_row_style(&self) -> PlaylistRowStyle;

    /// Возвращает explicit стиль Undo в заголовке Playlist.
    #[must_use]
    fn playlist_header_undo_style(&self) -> PlaylistHeaderUndoStyle;

    /// Возвращает explicit стиль иконного toolbar плейлиста.
    #[must_use]
    fn playlist_toolbar_style(&self) -> PlaylistToolbarStyle;

    /// Возвращает frame нижней панели.
    fn bottom_panel_frame(&self) -> Frame {
        Frame::NONE
            .fill(self.controls_style().bottom_panel_fill)
            .inner_margin(self.controls_style().panel_margin)
    }

    /// Возвращает цвет затемнения stale video frame.
    #[must_use]
    fn stale_frame_dim_color(&self, animation_state: AnimationState) -> Option<Color32>;
}

/// Выбирает skin по validated config id.
#[must_use]
pub fn skin_from_config(raw_skin_id: &str) -> Option<MinimalSkin> {
    SkinId::parse(raw_skin_id).map(|skin_id| match skin_id {
        SkinId::Minimal => MinimalSkin,
    })
}

#[cfg(test)]
mod tests {
    use egui::Color32;

    use super::{MINIMAL_SKIN_ID, MinimalSkin, SkinId, skin_from_config};
    use crate::ui::skin::PlayerSkin;

    /// Проверяет, что config id `minimal` выбирает первый skin.
    #[test]
    fn minimal_config_id_selects_minimal_skin() {
        let skin = skin_from_config(MINIMAL_SKIN_ID).expect("minimal skin resolved");

        assert_eq!(skin.id(), SkinId::Minimal);
    }

    /// Проверяет, что неизвестный id не считается валидным skin-ом UI.
    #[test]
    fn unknown_skin_id_is_not_resolved() {
        assert_eq!(SkinId::parse("dense"), None);
    }

    /// Проверяет, что minimal skin включает тёмную outline-защиту timeline.
    #[test]
    fn minimal_timeline_style_has_dark_nonzero_outline() {
        let timeline_style = MinimalSkin.timeline_style();
        let expected_outline_fill = Color32::from_rgb(18, 18, 18);

        assert!(timeline_style.track_outline_width > 0.0);
        assert!(timeline_style.thumb_outline_width > 0.0);
        assert_eq!(timeline_style.track_outline_fill, expected_outline_fill);
        assert_eq!(timeline_style.thumb_outline_fill, expected_outline_fill);
    }

    /// Playlist row tokens закрепляют белые alpha-состояния и отсутствие blue accent.
    #[test]
    fn minimal_playlist_rows_use_exact_white_alpha_tokens_without_blue() {
        let style = MinimalSkin.playlist_row_style();
        assert_eq!(
            style.hover_fill,
            Color32::from_rgba_unmultiplied(255, 255, 255, 28)
        );
        assert_eq!(
            style.selected_fill,
            Color32::from_rgba_unmultiplied(255, 255, 255, 46)
        );
        assert_eq!(
            style.selected_hover_fill,
            Color32::from_rgba_unmultiplied(255, 255, 255, 64)
        );
        assert_eq!(
            style.active_marker,
            ui_artwork_egui::PlaylistRowMarkerStyle {
                width: 3.0,
                vertical_inset: 4.0,
                corner_radius: 1.5,
                fill: Color32::from_rgba_unmultiplied(245, 245, 245, 235),
            }
        );
        assert_eq!(style.active_title_color, Color32::from_rgb(245, 245, 245));
        assert_eq!(
            style.separator_color,
            Color32::from_rgba_unmultiplied(255, 255, 255, 128)
        );
        for color in [
            style.insertion_stroke.color,
            style.focus_stroke.color,
            style.active_marker.fill,
            style.active_title_color,
        ] {
            assert_eq!(color.r(), color.g());
            assert_eq!(color.g(), color.b());
        }
    }

    /// Toolbar совпадает по визуальному весу с transport-кнопками и не имеет цветного акцента.
    #[test]
    fn minimal_playlist_toolbar_matches_transport_weight_without_color_accent() {
        let style = MinimalSkin.playlist_toolbar_style();
        let controls_style = MinimalSkin.controls_style();

        assert_eq!(style.button_size, controls_style.transport_button_size);
        assert_eq!(controls_style.left_edge_control_center_step, 40.0);
        assert_eq!(style.button_gap, 8.0);
        assert_eq!(style.left_group_padding, 23.0);
        assert_eq!(style.clear_right_padding, 18.0);
        assert_eq!(style.icon_extent, 23.5);
        assert_eq!(
            style.glyph_stroke_width,
            controls_style.playback_button_stroke_width
        );
        assert_eq!(style.foreground_idle, controls_style.text_color);
        for color in [
            style.foreground_idle,
            style.foreground_hover,
            style.foreground_disabled,
            style.surface_hover,
            style.surface_pressed,
            style.focus_outline.color,
        ] {
            assert_eq!(color.r(), color.g());
            assert_eq!(color.g(), color.b());
        }
    }

    /// Header Undo использует отдельный skin contract и точный heading-scale.
    #[test]
    fn minimal_playlist_header_undo_uses_heading_height_and_stronger_stroke() {
        let style = MinimalSkin.playlist_header_undo_style();

        assert_eq!(style.hit_area_size, 32.0);
        assert_eq!(style.glyph_text_style, egui::TextStyle::Heading);
        assert_eq!(style.glyph_stroke_width, 2.0);
        for color in [
            style.foreground_idle,
            style.foreground_hover,
            style.foreground_disabled,
            style.surface_hover,
            style.surface_pressed,
            style.focus_outline.color,
        ] {
            assert_eq!(color.r(), color.g());
            assert_eq!(color.g(), color.b());
        }
    }
}
