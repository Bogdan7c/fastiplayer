//! Единый app-level host левого sidebar.
//!
//! Этот модуль создаёт ровно один egui Panel. Playlist, Settings, URL и Info
//! являются только сменяемым содержимым и не владеют шириной или resize-state.

use egui::{Button, RichText, Ui};

use crate::settings_ui::{SettingsUiAction, SettingsUiModel, layout};
use crate::state::{ContentSlideDirection, SidebarContentTransition, SidebarSection};
use crate::ui::media_info;

const DEFAULT_SIDEBAR_WIDTH: f32 = 420.0;
const MIN_SIDEBAR_WIDTH: f32 = 320.0;
const MAX_SIDEBAR_WIDTH: f32 = 560.0;
const SIDEBAR_FILL: egui::Color32 = egui::Color32::from_rgb(18, 18, 18);

/// Все borrowed данные сменяемого содержимого sidebar.
pub(crate) struct SidebarRenderContext<'a> {
    pub(crate) model: &'a SettingsUiModel,
    pub(crate) snapshot: &'a player_core::PlayerSnapshot,
    pub(crate) settings_actions: &'a mut Vec<SettingsUiAction>,
    pub(crate) close_requested: &'a mut bool,
}

/// Единый ID: изменение секции не создаёт новый persisted width-state.
fn sidebar_panel_id() -> egui::Id {
    egui::Id::new("app_sidebar")
}

/// Последняя пользовательская ширина для open/close animation.
fn sidebar_open_width_memory_id() -> egui::Id {
    egui::Id::new("app_sidebar_open_width")
}

/// Рисует единственный sidebar и возвращает rect, который вытесняет видео.
#[must_use]
pub(crate) fn show(
    ui: &mut Ui,
    displayed_section: Option<SidebarSection>,
    slide_progress: f32,
    content_transition: Option<SidebarContentTransition>,
    mut context: SidebarRenderContext<'_>,
) -> Option<egui::Rect> {
    let displayed_section = displayed_section?;
    if slide_progress <= 0.0 {
        return None;
    }

    let target_width = remembered_width(ui);
    let fully_open = slide_progress >= 1.0;
    let visible_width = (target_width * slide_progress).max(1.0);
    let panel = egui::Panel::left(sidebar_panel_id())
        .resizable(fully_open)
        .default_size(target_width)
        .size_range(if fully_open {
            MIN_SIDEBAR_WIDTH..=MAX_SIDEBAR_WIDTH
        } else {
            visible_width..=visible_width
        })
        .frame(egui::Frame::NONE.fill(SIDEBAR_FILL));

    let response = panel.show_inside(ui, |ui| {
        // Содержимое не имеет права задавать minimum width владельцу Panel.
        ui.set_min_width(0.0);
        let panel_rect = ui.max_rect();
        match content_transition {
            Some(transition) => render_content_transition(ui, panel_rect, transition, &mut context),
            None => render_open_content(
                ui,
                panel_rect,
                target_width,
                displayed_section,
                fully_open,
                &mut context,
            ),
        }
    });

    let sidebar_rect = response.response.rect;
    if fully_open {
        ui.ctx().data_mut(|data| {
            data.insert_temp(
                sidebar_open_width_memory_id(),
                sidebar_rect
                    .width()
                    .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH),
            );
        });
    }
    Some(sidebar_rect)
}

fn remembered_width(ui: &Ui) -> f32 {
    ui.ctx()
        .data(|data| data.get_temp::<f32>(sidebar_open_width_memory_id()))
        .unwrap_or(DEFAULT_SIDEBAR_WIDTH)
        .clamp(MIN_SIDEBAR_WIDTH, MAX_SIDEBAR_WIDTH)
}

/// В fully-open состоянии содержимое рисуется прямо в Panel UI, чтобы его
/// текущий max rect не превращался в minimum width и не блокировал resize.
/// Fixed child используется только во время open/close animation.
fn render_open_content(
    ui: &mut Ui,
    panel_rect: egui::Rect,
    target_width: f32,
    section: SidebarSection,
    fully_open: bool,
    context: &mut SidebarRenderContext<'_>,
) {
    let Some(content_rect) = animated_content_rect(panel_rect, target_width, fully_open) else {
        render_section(ui, section, context);
        return;
    };
    let mut child = content_child(ui, panel_rect, content_rect, ("open", section as u8));
    child.disable();
    render_section(&mut child, section, context);
}

/// `None` означает direct rendering в resizable host. Fixed rect разрешён
/// только для промежуточной open/close animation.
fn animated_content_rect(
    panel_rect: egui::Rect,
    target_width: f32,
    fully_open: bool,
) -> Option<egui::Rect> {
    (!fully_open).then(|| {
        egui::Rect::from_min_max(
            egui::pos2(panel_rect.right() - target_width, panel_rect.top()),
            panel_rect.max,
        )
    })
}

/// Рисует outgoing/incoming content внутри того же Panel.
fn render_content_transition(
    ui: &mut Ui,
    panel_rect: egui::Rect,
    transition: SidebarContentTransition,
    context: &mut SidebarRenderContext<'_>,
) {
    let width = panel_rect.width();
    let progress = animation_core::Easing::EaseInOutCubic.apply(transition.progress);
    let travel = width * progress;
    let (outgoing_offset, incoming_offset) = match transition.direction {
        ContentSlideDirection::FromRight => (-travel, width - travel),
        ContentSlideDirection::FromLeft => (travel, -width + travel),
    };

    for (role, section, offset) in [
        ("outgoing", transition.from, outgoing_offset),
        ("incoming", transition.to, incoming_offset),
    ] {
        let content_rect = panel_rect.translate(egui::vec2(offset, 0.0));
        let mut child = content_child(ui, panel_rect, content_rect, (role, section as u8));
        // Переходящие visual-копии не принимают input.
        child.disable();
        render_section(&mut child, section, context);
    }
}

fn content_child(
    ui: &mut Ui,
    panel_rect: egui::Rect,
    content_rect: egui::Rect,
    id_salt: impl std::hash::Hash,
) -> Ui {
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .id_salt(id_salt)
            .max_rect(content_rect),
    );
    child.set_clip_rect(panel_rect.intersect(ui.clip_rect()));
    child.set_min_width(0.0);
    child
}

/// Единственная точка выбора содержимого; Panel state здесь отсутствует.
fn render_section(ui: &mut Ui, section: SidebarSection, context: &mut SidebarRenderContext<'_>) {
    ui.push_id(("section", section as u8), |ui| match section {
        SidebarSection::Playlist => {
            render_simple_header(ui, "Плейлист", context.close_requested);
        }
        SidebarSection::Settings => {
            render_settings_header(ui, context.settings_actions, context.close_requested);
            ui.separator();
            layout::show(ui, context.model, context.settings_actions);
        }
        SidebarSection::Url => {
            render_simple_header(ui, "URL", context.close_requested);
        }
        SidebarSection::Info => {
            render_simple_header(ui, "Информация", context.close_requested);
            ui.separator();
            egui::ScrollArea::vertical()
                .id_salt("info_scroll")
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    ui.set_min_width(0.0);
                    media_info::show(ui, context.snapshot);
                });
        }
    });
}

/// Settings X остаётся explicit Cancel/rollback.
fn render_settings_header(
    ui: &mut Ui,
    actions: &mut Vec<SettingsUiAction>,
    close_requested: &mut bool,
) {
    ui.horizontal(|ui| {
        ui.label(RichText::new("Настройки").heading());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(Button::new(RichText::new("×").strong()))
                .on_hover_text("Отменить изменения и закрыть настройки")
                .clicked()
            {
                actions.push(settings_sidebar_close_action());
                *close_requested = true;
            }
        });
    });
}

/// Остальные X только скрывают host.
fn render_simple_header(ui: &mut Ui, title: &str, close_requested: &mut bool) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).heading());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .add(Button::new(RichText::new("×").strong()))
                .on_hover_text("Закрыть панель")
                .clicked()
            {
                *close_requested = true;
            }
        });
    });
}

#[must_use]
fn settings_sidebar_close_action() -> SettingsUiAction {
    SettingsUiAction::Cancel
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn settings_sidebar_close_maps_to_cancel() {
        assert_eq!(settings_sidebar_close_action(), SettingsUiAction::Cancel);
    }

    #[test]
    fn sidebar_source_has_exactly_one_panel_creation_site() {
        let source = sidebar_source();
        let panel_constructor = format!("{}{}", "Panel::", "left(");
        let shared_constructor = format!("{}{}", "Panel::left", "(sidebar_panel_id())");
        assert_eq!(source.matches(&panel_constructor).count(), 1);
        assert!(source.contains(&shared_constructor));
    }

    #[test]
    fn all_sections_share_one_width_policy() {
        assert_eq!(DEFAULT_SIDEBAR_WIDTH, 420.0);
        assert_eq!(MIN_SIDEBAR_WIDTH, 320.0);
        assert_eq!(MAX_SIDEBAR_WIDTH, 560.0);
    }

    #[test]
    fn fully_open_content_does_not_create_width_locking_child_rect() {
        let panel_rect = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(420.0, 700.0));
        assert_eq!(animated_content_rect(panel_rect, 420.0, true), None);
        assert!(animated_content_rect(panel_rect, 420.0, false).is_some());
    }

    #[test]
    fn source_guardrail_sidebar_stays_visual_only() {
        let normalized_source = sidebar_source().to_lowercase();
        let forbidden_patterns = [
            concat!("settings_", "runtime").to_string(),
            concat!("rustiplayer_", "config").to_string(),
            format!("{}{}", "app", "config"),
            concat!("render_", "wg", "pu").to_string(),
            concat!("render-", "wg", "pu").to_string(),
            concat!("wg", "pu").to_string(),
        ];
        for forbidden_pattern in forbidden_patterns {
            assert!(
                !normalized_source.contains(&forbidden_pattern),
                "ui/sidebar.rs must not reference {forbidden_pattern}"
            );
        }
    }

    fn sidebar_source() -> String {
        let sidebar_path = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/ui/sidebar.rs");
        std::fs::read_to_string(sidebar_path).expect("sidebar source is readable")
    }
}
