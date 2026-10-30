//! Компактный переключатель секций в левой части custom titlebar.

use egui::{Color32, Rect, Sense, Stroke, Ui, WidgetInfo, WidgetType, pos2, vec2};
use ui_artwork_egui::{ArtworkPainter, ButtonVisualState, SidebarButtonGlyph};

use crate::state::SidebarSection;

const BUTTON_WIDTH: f32 = 36.0;
const BUTTON_HEIGHT: f32 = 32.0;
const ACTIVE_FILL_ALPHA: u8 = 20;
const ACTIVE_HOVER_FILL_ALPHA: u8 = 42;

/// Подсказка и имя кнопки секции (то же имя читает доступность).
const fn section_tooltip(section: SidebarSection) -> &'static str {
    match section {
        SidebarSection::Playlist => "Playlist",
        SidebarSection::Settings => "Settings",
        SidebarSection::Url => "URL",
        SidebarSection::Info => "Info",
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TitlebarIconAreaAction {
    SelectSidebarSection(SidebarSection),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TitlebarIconAreaStyle {
    pub(crate) icon_stroke: Stroke,
    pub(crate) button_hover_fill: Color32,
}

/// Горизонтальная сетка центров левой titlebar-группы.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct TitlebarIconAreaAlignment {
    /// Абсолютная X-координата центра первой кнопки.
    first_button_center_x: f32,

    /// Расстояние между центрами соседних кнопок.
    button_center_step: f32,
}

impl TitlebarIconAreaAlignment {
    /// Создаёт explicit alignment без скрытого знания о соседних UI-модулях.
    #[must_use]
    pub(crate) fn new(first_button_center_x: f32, button_center_step: f32) -> Self {
        Self {
            first_button_center_x,
            button_center_step,
        }
    }

    /// Возвращает центр кнопки на месте `slot` (0 — первая слева).
    #[must_use]
    fn slot_center_x(self, slot: usize) -> f32 {
        self.first_button_center_x + self.button_center_step * slot as f32
    }
}

/// Типизированно связывает sidebar intent со стабильной window-chrome осью.
const fn section_axis_index(section: SidebarSection) -> usize {
    match section {
        SidebarSection::Playlist => 0,
        SidebarSection::Settings => 1,
        SidebarSection::Url => 2,
        SidebarSection::Info => 3,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct TitlebarIconAreaOutput {
    pub(crate) reserved_rect: Rect,
    pub(crate) actions: Vec<TitlebarIconAreaAction>,
}

#[must_use]
pub(crate) fn show(
    ui: &mut Ui,
    titlebar_rect: Rect,
    alignment: TitlebarIconAreaAlignment,
    style: TitlebarIconAreaStyle,
    active_section: Option<SidebarSection>,
    sections: &[SidebarSection],
) -> TitlebarIconAreaOutput {
    let mut actions = Vec::new();
    // Кнопки идут подряд в порядке секций вида окна: в окне без очереди и URL
    // (браузерный режим) Settings и Info стоят на первых местах, без дыр. Обычное окно
    // предлагает все секции, и место каждой совпадает с её осью (`section_axis_index`).
    for (slot, &section) in sections.iter().enumerate() {
        let tooltip = section_tooltip(section);
        let button_rect = button_rect_at_slot(titlebar_rect, alignment, slot);
        let response = ui
            .interact(
                button_rect,
                ui.id().with(("sidebar_section", section as u8)),
                Sense::click(),
            )
            .on_hover_text(tooltip);
        response.widget_info(|| WidgetInfo::labeled(WidgetType::Button, ui.is_enabled(), tooltip));
        paint_button(
            ui,
            button_rect,
            section,
            active_section == Some(section),
            response.hovered(),
            style,
        );
        if response.clicked() {
            actions.push(TitlebarIconAreaAction::SelectSidebarSection(section));
        }
    }

    TitlebarIconAreaOutput {
        reserved_rect: reserved_rect(titlebar_rect, alignment),
        actions,
    }
}

/// Вычисляет кнопку относительно общей оси первой titlebar-иконки.
///
/// Все последующие кнопки используют skin-owned center step, поэтому titlebar
/// и playlist toolbar могут разделять одну горизонтальную сетку.
#[must_use]
pub(crate) fn button_rect(
    titlebar_rect: Rect,
    alignment: TitlebarIconAreaAlignment,
    section: SidebarSection,
) -> Rect {
    button_rect_at_slot(titlebar_rect, alignment, section_axis_index(section))
}

/// Кнопка на месте `slot` общей оси titlebar-иконок.
fn button_rect_at_slot(
    titlebar_rect: Rect,
    alignment: TitlebarIconAreaAlignment,
    slot: usize,
) -> Rect {
    let top = titlebar_rect.center().y - BUTTON_HEIGHT * 0.5;
    let left = alignment.slot_center_x(slot) - BUTTON_WIDTH * 0.5;
    Rect::from_min_size(pos2(left, top), vec2(BUTTON_WIDTH, BUTTON_HEIGHT))
}

/// Возвращает общий hit-rect всей левой группы, не захватывая край окна.
#[must_use]
pub(crate) fn button_group_rect(titlebar_rect: Rect, alignment: TitlebarIconAreaAlignment) -> Rect {
    let first_button_rect = button_rect(titlebar_rect, alignment, SidebarSection::Playlist);
    let last_button_rect = button_rect(titlebar_rect, alignment, SidebarSection::Info);

    Rect::from_min_max(first_button_rect.min, last_button_rect.max)
}

#[must_use]
pub(crate) fn reserved_rect(titlebar_rect: Rect, alignment: TitlebarIconAreaAlignment) -> Rect {
    let button_group_rect = button_group_rect(titlebar_rect, alignment);

    Rect::from_min_max(
        titlebar_rect.min,
        pos2(button_group_rect.right(), titlebar_rect.bottom()),
    )
}

fn paint_button(
    ui: &Ui,
    rect: Rect,
    section: SidebarSection,
    active: bool,
    hovered: bool,
    style: TitlebarIconAreaStyle,
) {
    let active_fill = Color32::from_rgba_unmultiplied(
        255,
        255,
        255,
        if hovered {
            ACTIVE_HOVER_FILL_ALPHA
        } else {
            ACTIVE_FILL_ALPHA
        },
    );
    let painter = ArtworkPainter::new(ui.painter());
    if active {
        painter.sidebar_button_active_background(rect, active_fill);
    }
    let visual_state = if hovered {
        ButtonVisualState::Hovered
    } else {
        ButtonVisualState::Idle
    };
    match section {
        SidebarSection::Settings => painter.settings_button(
            rect,
            visual_state,
            style.icon_stroke,
            style.button_hover_fill,
        ),
        SidebarSection::Playlist => painter.sidebar_button(
            rect,
            SidebarButtonGlyph::Playlist,
            visual_state,
            style.icon_stroke,
            style.button_hover_fill,
        ),
        SidebarSection::Url => painter.sidebar_button(
            rect,
            SidebarButtonGlyph::Url,
            visual_state,
            style.icon_stroke,
            style.button_hover_fill,
        ),
        SidebarSection::Info => painter.sidebar_button(
            rect,
            SidebarButtonGlyph::Info,
            visual_state,
            style.icon_stroke,
            style.button_hover_fill,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rects_have_required_order_size_and_spacing() {
        let titlebar = Rect::from_min_size(pos2(10.0, 5.0), vec2(900.0, 64.0));
        let first_button_center_x = titlebar.left() + 39.0;
        let alignment = TitlebarIconAreaAlignment::new(first_button_center_x, 40.0);
        let rects = [
            SidebarSection::Playlist,
            SidebarSection::Settings,
            SidebarSection::Url,
            SidebarSection::Info,
        ]
        .map(|section| button_rect(titlebar, alignment, section));
        assert_eq!(rects[0].center().x, first_button_center_x);
        assert!(rects.iter().all(|rect| rect.size() == vec2(36.0, 32.0)));
        assert!(
            rects
                .windows(2)
                .all(|pair| pair[1].left() - pair[0].right() == 4.0)
        );
        assert_eq!(rects[0].center().y, titlebar.center().y);
    }

    #[test]
    fn reserved_area_covers_inset_gaps_and_every_button() {
        let titlebar = Rect::from_min_size(pos2(10.0, 5.0), vec2(900.0, 32.0));
        let first_button_center_x = titlebar.left() + 39.0;
        let alignment = TitlebarIconAreaAlignment::new(first_button_center_x, 40.0);
        let reserved = reserved_rect(titlebar, alignment);
        let button_group = button_group_rect(titlebar, alignment);
        assert_eq!(reserved.left(), titlebar.left());
        assert_eq!(reserved.right(), button_group.right());
        assert_eq!(
            button_group.left(),
            button_rect(titlebar, alignment, SidebarSection::Playlist).left()
        );
        assert_eq!(
            button_group.right(),
            button_rect(titlebar, alignment, SidebarSection::Info).right()
        );
    }

    /// Кнопки, которые titlebar отдаёт доступности, и их центры по X.
    fn rendered_buttons(sections: &[SidebarSection]) -> Vec<(String, f32)> {
        let context = egui::Context::default();
        context.enable_accesskit();
        let titlebar = Rect::from_min_size(pos2(0.0, 0.0), vec2(800.0, 40.0));
        let alignment = TitlebarIconAreaAlignment::new(39.0, 40.0);
        let style = TitlebarIconAreaStyle {
            icon_stroke: Stroke::new(1.0, Color32::WHITE),
            button_hover_fill: Color32::DARK_GRAY,
        };
        let output = crate::ui::test_frame::run_ui_frame(
            &context,
            crate::ui::test_frame::input_at(0.0, Vec::new()),
            |ui| {
                let _ = show(ui, titlebar, alignment, style, None, sections);
            },
        );
        let update = output
            .platform_output
            .accesskit_update
            .expect("AccessKit tree update");
        let mut buttons: Vec<(String, f32)> = update
            .nodes
            .iter()
            .filter(|(_, node)| node.role() == egui::accesskit::Role::Button)
            .filter_map(|(_, node)| {
                let bounds = node.bounds()?;
                Some((
                    node.label()?.to_owned(),
                    ((bounds.x0 + bounds.x1) / 2.0) as f32,
                ))
            })
            .collect();
        buttons.sort_by(|left, right| left.1.total_cmp(&right.1));
        buttons
    }

    /// Обычное окно: все четыре секции на своих осях (вид не изменился).
    #[test]
    fn the_standard_window_offers_every_sidebar_section() {
        let buttons = rendered_buttons(
            crate::window_view_profile::WindowViewProfile::Standard.sidebar_sections(),
        );
        let labels: Vec<&str> = buttons.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, ["Playlist", "Settings", "URL", "Info"]);
        assert_eq!(buttons[0].1, 39.0);
        assert_eq!(buttons[3].1, 39.0 + 3.0 * 40.0);
    }

    /// Окно браузерного режима: ни очереди, ни URL; Settings и Info — на первых местах.
    #[test]
    fn the_browser_window_offers_only_settings_and_info_without_gaps() {
        let buttons = rendered_buttons(
            crate::window_view_profile::WindowViewProfile::BrowserSession.sidebar_sections(),
        );
        let labels: Vec<&str> = buttons.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, ["Settings", "Info"]);
        assert_eq!(buttons[0].1, 39.0);
        assert_eq!(buttons[1].1, 39.0 + 40.0);
    }
}
