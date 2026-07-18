//! Privacy-safe read-only status formatting для Playlist sidebar.

use super::PlaylistUiState;
use crate::playlist_runtime::{
    PlaylistLoadingView, PlaylistNavigationView, PlaylistProbeView, PlaylistSaveView,
    PlaylistStartupWarningView, PlaylistViewModel,
};

pub(super) fn show_unavailable(ui: &mut egui::Ui) {
    ui.label("Плейлист ещё подключается…");
}

pub(super) fn show_summary(
    ui: &mut egui::Ui,
    model: &PlaylistViewModel,
    state: &mut PlaylistUiState,
) {
    ui.horizontal(|ui| {
        ui.strong("Очередь");
        ui.label(format!("{} элементов", model.item_count()));
    });

    if model.has_active_tombstone() {
        let response = ui.add(
            egui::Label::new(egui::RichText::new("Сейчас играет — удалено из очереди").strong())
                .sense(egui::Sense::focusable_noninteractive()),
        );
        if state.take_tombstone_request() {
            response.scroll_to_me(Some(egui::Align::Center));
            response.request_focus();
        }
    }
    if matches!(model.loading(), PlaylistLoadingView::Loading) {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Загрузка сохранённой очереди…");
        });
    } else if model.is_empty() {
        ui.label("Очередь пуста");
    }
    if !model.structural_actions_enabled() {
        ui.weak("Изменения очереди временно недоступны");
    }
    if matches!(model.startup_warning(), PlaylistStartupWarningView::Present) {
        ui.colored_label(
            ui.visuals().warn_fg_color,
            "Сохранённая очередь восстановлена с предупреждением",
        );
    }
    show_probe(ui, model.probe());
    show_save(ui, model.save());
    show_navigation(ui, model.navigation());
    ui.separator();
}

fn show_probe(ui: &mut egui::Ui, probe: PlaylistProbeView) {
    let message = match probe {
        PlaylistProbeView::Idle => return,
        PlaylistProbeView::Enumerating => "Поиск файлов рядом…".to_owned(),
        PlaylistProbeView::Probing { processed, total } => {
            format!("Проверка файлов: {processed} из {total}")
        }
        PlaylistProbeView::ManualProbe { processed, total } => {
            format!("Проверка добавляемых файлов: {processed} из {total}")
        }
        PlaylistProbeView::Completed => "Проверка файлов завершена".to_owned(),
        PlaylistProbeView::Warning => {
            "Добавлена только доступная часть очереди; проверка завершилась с предупреждением"
                .to_owned()
        }
    };
    ui.label(message);
}

fn show_save(ui: &mut egui::Ui, save: PlaylistSaveView) {
    let Some((message, tone)) = save_message(save) else {
        return;
    };
    match tone {
        StatusTone::Normal => {
            ui.weak(message);
        }
        StatusTone::Warning => {
            ui.colored_label(ui.visuals().warn_fg_color, message);
        }
        StatusTone::Error => {
            ui.colored_label(ui.visuals().error_fg_color, message);
        }
    }
}

fn show_navigation(ui: &mut egui::Ui, navigation: PlaylistNavigationView) {
    let Some((message, tone)) = navigation_message(navigation) else {
        return;
    };
    match tone {
        StatusTone::Normal => {
            ui.label(message);
        }
        StatusTone::Warning => {
            ui.colored_label(ui.visuals().warn_fg_color, message);
        }
        StatusTone::Error => {
            ui.colored_label(ui.visuals().error_fg_color, message);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StatusTone {
    Normal,
    Warning,
    Error,
}

pub(super) fn save_message(save: PlaylistSaveView) -> Option<(String, StatusTone)> {
    match save {
        // Обычное фоновое сохранение не требует действий пользователя и не должно мигать в UI.
        PlaylistSaveView::Idle | PlaylistSaveView::Saving => None,
        PlaylistSaveView::WarningRetryAvailable { occurrence_count } => Some((
            format!(
                "Не удалось сохранить очередь (попыток: {occurrence_count}). Повтор сохранения доступен"
            ),
            StatusTone::Warning,
        )),
        PlaylistSaveView::Blocked => Some((
            "Сохранение очереди заблокировано для защиты существующего файла".to_owned(),
            StatusTone::Warning,
        )),
        PlaylistSaveView::Fault(_fault) => Some((
            "Служба сохранения очереди недоступна".to_owned(),
            StatusTone::Error,
        )),
    }
}

pub(super) fn navigation_message(
    navigation: PlaylistNavigationView,
) -> Option<(&'static str, StatusTone)> {
    match navigation {
        PlaylistNavigationView::Idle => None,
        PlaylistNavigationView::WaitingForCandidate => Some((
            "Ожидание следующего доступного элемента…",
            StatusTone::Normal,
        )),
        PlaylistNavigationView::AwaitingUserAfterFailure => Some((
            "Переход не выполнен. Автоматический переход остановлен; Next, Previous или повтор продолжат сохранённый курсор",
            StatusTone::Error,
        )),
        PlaylistNavigationView::Exhausted => {
            Some(("Подходящих элементов больше нет", StatusTone::Normal))
        }
        PlaylistNavigationView::Cancelled => {
            Some(("Ожидание перехода отменено", StatusTone::Normal))
        }
        PlaylistNavigationView::Fatal => {
            Some(("Переход недоступен из-за ошибки службы", StatusTone::Error))
        }
    }
}
