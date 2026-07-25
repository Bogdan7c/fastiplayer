//! Содержимое URL-секции и typed same-item selection intents.

use egui::{RichText, Ui};
use web_media_core::{CodecFamily, ContainerFamily, StreamLayoutKind};

use crate::web_media_stream_model::component_variants::WebMediaComponentVariantProjection;
use crate::web_media_stream_model::{
    UrlSidebarAction, UrlSidebarItemScope, UrlSidebarModel, UrlSidebarPendingSelection,
    UrlSidebarPlaybackStatus, UrlSidebarSafeError, WebMediaCandidatePresentation,
    WebMediaSelectionPreference, WebMediaStreamGeneration,
};

mod component_variants;
#[cfg(test)]
mod component_variants_tests;

/// Рисует active web-media configuration и возвращает один same-item intent.
pub(crate) fn show(ui: &mut Ui, model: &UrlSidebarModel) -> Option<UrlSidebarAction> {
    egui::ScrollArea::vertical()
        .id_salt("url_stream_configuration_scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(0.0);
            match model {
                UrlSidebarModel::Inactive => {
                    show_inactive(ui);
                    None
                }
                UrlSidebarModel::DirectMedia {
                    source_label,
                    status,
                } => {
                    show_direct_media(ui, source_label, *status);
                    None
                }
                UrlSidebarModel::YtDlp {
                    generation,
                    source_label,
                    candidates,
                    active_candidate,
                    pending_selection,
                    component_variants,
                    preference,
                    item_scope,
                    status,
                    safe_error,
                } => show_yt_dlp(
                    ui,
                    *generation,
                    source_label,
                    candidates,
                    active_candidate,
                    pending_selection.as_deref(),
                    component_variants,
                    *preference,
                    *item_scope,
                    *status,
                    *safe_error,
                ),
            }
        })
        .inner
}

fn show_inactive(ui: &mut Ui) {
    ui.heading("Поток");
    ui.label("Сейчас активно локальное медиа или источник ещё не установлен.");
    ui.add_space(8.0);
    ui.weak("URL добавляется только через кнопку «Добавить URL» в плейлисте.");
}

fn show_direct_media(ui: &mut Ui, source_label: &str, status: UrlSidebarPlaybackStatus) {
    ui.heading("Прямой web-поток");
    wrapped_value(ui, "Источник", source_label);
    ui.add_space(8.0);
    status_grid(ui, status);
    ui.add_space(8.0);
    ui.label(
        "Источник содержит один прямой media resource. Выбор разрешения и формата недоступен.",
    );
}

#[allow(clippy::too_many_arguments)]
fn show_yt_dlp(
    ui: &mut Ui,
    generation: WebMediaStreamGeneration,
    source_label: &str,
    candidates: &[WebMediaCandidatePresentation],
    active_candidate: &WebMediaCandidatePresentation,
    pending_selection: Option<&UrlSidebarPendingSelection>,
    component_variants: &WebMediaComponentVariantProjection,
    preference: WebMediaSelectionPreference,
    item_scope: UrlSidebarItemScope,
    status: UrlSidebarPlaybackStatus,
    safe_error: Option<UrlSidebarSafeError>,
) -> Option<UrlSidebarAction> {
    ui.heading("Web-медиа");
    wrapped_value(ui, "Источник", source_label);
    wrapped_value(ui, "Применение", item_scope_label(item_scope));
    wrapped_value(ui, "Предпочтение", &preference_label(preference));
    ui.add_space(8.0);
    status_grid(ui, status);

    if let Some(error) = safe_error {
        ui.add_space(8.0);
        ui.colored_label(ui.visuals().error_fg_color, safe_error_label(error));
        ui.weak("Повтор выполняется через controlled reopen и не меняет очередь напрямую.");
    }

    ui.add_space(12.0);
    ui.label(RichText::new(candidate_section_title(candidates)).strong());
    let mut action = None;
    if candidates.is_empty() {
        ui.label("Нет доступных playable форматов.");
    } else {
        for (candidate_index, candidate) in candidates.iter().enumerate() {
            let active = candidate == active_candidate;
            let pending = matches!(
                pending_selection,
                Some(UrlSidebarPendingSelection::Candidate {
                    candidate: pending_candidate,
                    ..
                }) if pending_candidate == candidate
            );
            if action.is_none()
                && show_candidate(ui, candidate, active, pending, pending_selection.is_some())
            {
                action = Some(UrlSidebarAction::SelectCandidate {
                    generation,
                    candidate_index,
                });
            }
        }
    }

    let component_action =
        component_variants::show(ui, generation, component_variants, pending_selection);
    choose_single_sidebar_action(action, component_action)
}

/// Один frame публикует не более одного intent; candidate сохраняет прежний приоритет.
fn choose_single_sidebar_action(
    candidate_action: Option<UrlSidebarAction>,
    component_action: Option<
        crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction,
    >,
) -> Option<UrlSidebarAction> {
    candidate_action.or(component_action.map(UrlSidebarAction::SelectComponentVariant))
}

fn status_grid(ui: &mut Ui, status: UrlSidebarPlaybackStatus) {
    egui::Grid::new("url_stream_status_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.weak("Режим");
            ui.label(if status.is_live { "Live" } else { "VOD" });
            ui.end_row();

            ui.weak("Перемотка");
            ui.label(if status.seekable {
                "Доступна"
            } else {
                "Недоступна"
            });
            ui.end_row();

            ui.weak("Буферизация");
            ui.label(if status.buffering {
                "Идёт"
            } else {
                "Не требуется"
            });
            ui.end_row();

            ui.weak("Обновление");
            ui.label(if status.refresh_on_reopen {
                "При повторном открытии"
            } else {
                "Не требуется"
            });
            ui.end_row();
        });
}

fn show_candidate(
    ui: &mut Ui,
    candidate: &WebMediaCandidatePresentation,
    active: bool,
    pending: bool,
    switch_in_progress: bool,
) -> bool {
    ui.group(|ui| {
        ui.set_min_width(0.0);
        let state = if pending {
            "Ожидает переключения"
        } else if active {
            "Активный"
        } else {
            "Доступен"
        };
        ui.label(RichText::new(state).strong());
        ui.label(candidate_primary_label(candidate));
        ui.weak(candidate_format_label(candidate));
        ui.add_enabled(
            !active && !switch_in_progress,
            egui::Button::new("Переключить"),
        )
        .clicked()
    })
    .inner
}

fn candidate_section_title(candidates: &[WebMediaCandidatePresentation]) -> &'static str {
    if candidates.iter().all(|candidate| !candidate.has_video()) {
        "Доступные форматы"
    } else {
        "Доступные разрешения и форматы"
    }
}

fn candidate_primary_label(candidate: &WebMediaCandidatePresentation) -> String {
    match (candidate.width, candidate.height) {
        (Some(width), Some(height)) => format!("{width}×{height} ({height}p)"),
        (None, Some(height)) => format!("{height}p"),
        _ if candidate.layout == StreamLayoutKind::AudioOnly => "Только аудио".to_owned(),
        _ => "Разрешение не указано".to_owned(),
    }
}

fn candidate_format_label(candidate: &WebMediaCandidatePresentation) -> String {
    let mut parts = Vec::new();
    parts.push(layout_label(candidate.layout).to_owned());
    if let Some(container) = candidate.containers.video {
        parts.push(container_label(container).to_owned());
    }
    if candidate.containers.audio != candidate.containers.video
        && let Some(container) = candidate.containers.audio
    {
        parts.push(container_label(container).to_owned());
    }
    if let Some(codec) = candidate.video_codec {
        parts.push(codec_label(codec).to_owned());
    }
    if let Some(codec) = candidate.audio_codec {
        parts.push(codec_label(codec).to_owned());
    }
    if let Some((numerator, denominator)) = candidate.frame_rate
        && denominator != 0
    {
        parts.push(format!("{:.2} fps", numerator as f64 / denominator as f64));
    }
    if let Some(bits_per_second) = candidate.video_bitrate.or(candidate.audio_bitrate) {
        parts.push(format!("{} кбит/с", bits_per_second / 1_000));
    }
    parts.join(" • ")
}

fn wrapped_value(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.weak(format!("{label}:"));
        ui.label(value);
    });
}

fn preference_label(preference: WebMediaSelectionPreference) -> String {
    match preference {
        WebMediaSelectionPreference::GlobalBestPlayable => "Глобально: лучший доступный".to_owned(),
        WebMediaSelectionPreference::GlobalPreferredHeight(height) => {
            format!("Глобально: предпочитать {height}p")
        }
        WebMediaSelectionPreference::ItemOverride(None) => {
            "Для этого элемента: лучший доступный".to_owned()
        }
        WebMediaSelectionPreference::ItemOverride(Some(height)) => {
            format!("Для этого элемента: {height}p")
        }
    }
}

fn item_scope_label(scope: UrlSidebarItemScope) -> &'static str {
    match scope {
        UrlSidebarItemScope::Detached => "Вне текущей очереди",
        UrlSidebarItemScope::SingleItem => "Текущий элемент плейлиста",
        UrlSidebarItemScope::CompoundPart => "Текущая часть составного элемента",
    }
}

fn safe_error_label(error: UrlSidebarSafeError) -> &'static str {
    match error {
        UrlSidebarSafeError::SourceUnavailable => "Web-источник временно недоступен.",
        UrlSidebarSafeError::SameItemSwitchBusy => "Переключение уже выполняется.",
        UrlSidebarSafeError::SameItemSwitchStale => {
            "Список вариантов устарел; выберите нужную строку ещё раз."
        }
        UrlSidebarSafeError::SameItemSwitchCancelled => "Переключение отменено.",
    }
}

fn layout_label(layout: StreamLayoutKind) -> &'static str {
    match layout {
        StreamLayoutKind::Muxed => "Видео + аудио",
        StreamLayoutKind::Separate => "Раздельные видео/аудио",
        StreamLayoutKind::VideoOnly => "Только видео",
        StreamLayoutKind::AudioOnly => "Только аудио",
    }
}

fn container_label(container: ContainerFamily) -> &'static str {
    match container {
        ContainerFamily::IsoBmff => "MP4/M4A",
        ContainerFamily::FragmentedIsoBmff => "fMP4/CMAF",
        ContainerFamily::Matroska => "Matroska",
        ContainerFamily::WebM => "WebM",
        ContainerFamily::Ogg => "Ogg",
        ContainerFamily::Flac => "FLAC",
        ContainerFamily::Wav => "WAV",
        ContainerFamily::Aiff => "AIFF",
        ContainerFamily::Caf => "CAF",
        ContainerFamily::MpegAudio => "MPEG Audio",
        ContainerFamily::MpegTs => "MPEG-TS",
        ContainerFamily::Flv => "FLV",
        ContainerFamily::F4f => "F4F",
        ContainerFamily::MpegProgramStream => "MPEG-PS",
        ContainerFamily::Avi => "AVI",
        ContainerFamily::Asf => "ASF",
        ContainerFamily::Unknown => "Неизвестный контейнер",
    }
}

fn codec_label(codec: CodecFamily) -> &'static str {
    match codec {
        CodecFamily::Vp8 => "VP8",
        CodecFamily::Vp9 => "VP9",
        CodecFamily::Av1 => "AV1",
        CodecFamily::H264 => "H.264",
        CodecFamily::H265 => "H.265",
        CodecFamily::Opus => "Opus",
        CodecFamily::Vorbis => "Vorbis",
        CodecFamily::Aac => "AAC",
        CodecFamily::IsoBmffAudio => "ISO BMFF Audio",
        CodecFamily::Adpcm => "ADPCM",
        CodecFamily::Alac => "ALAC",
        CodecFamily::Flac => "FLAC",
        CodecFamily::Mp1 => "MP1",
        CodecFamily::Mp2 => "MP2",
        CodecFamily::Mp3 => "MP3",
        CodecFamily::Pcm => "PCM",
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn url_content_does_not_create_second_sidebar_panel() {
        let source = include_str!("url_sidebar.rs");
        let panel_constructor_prefix = format!("{}{}", "Panel", "::");
        assert_eq!(source.matches(&panel_constructor_prefix).count(), 0);
    }

    #[test]
    fn candidate_action_wins_if_candidate_and_component_click_in_same_frame() {
        let generation = crate::web_media_stream_model::WebMediaStreamGeneration::for_test(3, 5);
        let candidate_action = crate::web_media_stream_model::UrlSidebarAction::SelectCandidate {
            generation,
            candidate_index: 2,
        };
        let component_action =
            crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction {
                parent_generation: generation,
                catalog_generation: web_media_core::ComponentVariantCatalogGeneration::new(8),
                component: web_media_core::ComponentKind::Audio,
                variant_index: 1,
            };

        assert_eq!(
            super::choose_single_sidebar_action(Some(candidate_action), Some(component_action),),
            Some(candidate_action)
        );
    }
}
