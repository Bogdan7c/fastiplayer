//! Safe renderer независимых video/audio и неделимой coupled A/V axes.
//!
//! Модуль создаёт только generation-fenced row action. Exact identities,
//! semantic metadata и controlled reopen lifecycle остаются за model/state boundary.

use egui::{Button, Response, RichText, Ui};
use web_media_core::{ComponentVariantCatalogGeneration, DynamicRange};

use crate::web_media_stream_model::component_variants::{
    ComponentVariantSelectionAction, WebMediaAudioComponentVariantAxis,
    WebMediaAudioComponentVariantPresentation, WebMediaComponentVariantAxisKind,
    WebMediaComponentVariantProjection, WebMediaCoupledComponentVariantAxis,
    WebMediaCoupledComponentVariantPresentation, WebMediaInstalledComponentVariantPresentation,
    WebMediaVideoComponentVariantAxis, WebMediaVideoComponentVariantPresentation,
};
use crate::web_media_stream_model::{UrlSidebarPendingSelection, WebMediaStreamGeneration};

use super::codec_label;

pub(super) const VIDEO_HEADING: &str = "Видео";
pub(super) const AUDIO_HEADING: &str = "Аудио";
pub(super) const COUPLED_HEADING: &str = "Видео и аудио";
pub(super) const UNAVAILABLE_TEXT: &str = "Раздельный выбор недоступен для этого формата";
pub(super) const VIDEO_AXIS_MISSING_TEXT: &str = "В этом формате нет отдельной видеодорожки";
pub(super) const AUDIO_AXIS_MISSING_TEXT: &str = "В этом формате нет отдельной аудиодорожки";
const ACTIVE_VARIANT_TEXT: &str = "Этот вариант уже активен.";
const SWITCH_PENDING_TEXT: &str = "Сначала дождитесь завершения текущего переключения.";

/// Рисует две стабильные секции после candidate inventory.
pub(super) fn show(
    ui: &mut Ui,
    parent_generation: WebMediaStreamGeneration,
    projection: &WebMediaComponentVariantProjection,
    pending_selection: Option<&UrlSidebarPendingSelection>,
) -> Option<ComponentVariantSelectionAction> {
    ui.add_space(12.0);
    match projection {
        WebMediaComponentVariantProjection::Unavailable => {
            show_unavailable_axis(ui, VIDEO_HEADING);
            show_unavailable_axis(ui, AUDIO_HEADING);
            None
        }
        WebMediaComponentVariantProjection::Installed(presentation) => {
            show_installed(ui, parent_generation, presentation, pending_selection)
        }
    }
}

fn show_installed(
    ui: &mut Ui,
    parent_generation: WebMediaStreamGeneration,
    presentation: &WebMediaInstalledComponentVariantPresentation,
    pending_selection: Option<&UrlSidebarPendingSelection>,
) -> Option<ComponentVariantSelectionAction> {
    match presentation {
        WebMediaInstalledComponentVariantPresentation::VideoAndAudio {
            catalog_generation,
            video,
            audio,
        } => {
            let video_action = show_video_axis(
                ui,
                parent_generation,
                *catalog_generation,
                video,
                pending_selection,
            );
            let audio_action = show_audio_axis(
                ui,
                parent_generation,
                *catalog_generation,
                audio,
                pending_selection,
            );
            video_action.or(audio_action)
        }
        WebMediaInstalledComponentVariantPresentation::VideoOnly {
            catalog_generation,
            video,
        } => {
            let action = show_video_axis(
                ui,
                parent_generation,
                *catalog_generation,
                video,
                pending_selection,
            );
            show_missing_axis(ui, AUDIO_HEADING, AUDIO_AXIS_MISSING_TEXT);
            action
        }
        WebMediaInstalledComponentVariantPresentation::AudioOnly {
            catalog_generation,
            audio,
        } => {
            show_missing_axis(ui, VIDEO_HEADING, VIDEO_AXIS_MISSING_TEXT);
            show_audio_axis(
                ui,
                parent_generation,
                *catalog_generation,
                audio,
                pending_selection,
            )
        }
        WebMediaInstalledComponentVariantPresentation::Coupled {
            catalog_generation,
            coupled,
        } => show_coupled_axis(
            ui,
            parent_generation,
            *catalog_generation,
            coupled,
            pending_selection,
        ),
    }
}

fn show_unavailable_axis(ui: &mut Ui, heading: &str) {
    ui.label(RichText::new(heading).strong());
    ui.weak(UNAVAILABLE_TEXT);
    ui.add_space(6.0);
}

fn show_missing_axis(ui: &mut Ui, heading: &str, message: &str) {
    ui.label(RichText::new(heading).strong());
    ui.weak(message);
    ui.add_space(6.0);
}

fn show_video_axis(
    ui: &mut Ui,
    parent_generation: WebMediaStreamGeneration,
    catalog_generation: ComponentVariantCatalogGeneration,
    axis: &WebMediaVideoComponentVariantAxis,
    pending_selection: Option<&UrlSidebarPendingSelection>,
) -> Option<ComponentVariantSelectionAction> {
    ui.label(RichText::new(VIDEO_HEADING).strong());
    let mut action = None;
    for (variant_index, variant) in axis.variants.iter().enumerate() {
        let row_action = ComponentVariantSelectionAction {
            parent_generation,
            catalog_generation,
            axis: WebMediaComponentVariantAxisKind::Video,
            variant_index,
        };
        let clicked = show_variant_row(
            ui,
            row_action,
            variant_index == axis.active_index,
            video_label(variant),
            pending_selection,
        );
        if action.is_none() && clicked {
            action = Some(row_action);
        }
    }
    ui.add_space(6.0);
    action
}

fn show_audio_axis(
    ui: &mut Ui,
    parent_generation: WebMediaStreamGeneration,
    catalog_generation: ComponentVariantCatalogGeneration,
    axis: &WebMediaAudioComponentVariantAxis,
    pending_selection: Option<&UrlSidebarPendingSelection>,
) -> Option<ComponentVariantSelectionAction> {
    ui.label(RichText::new(AUDIO_HEADING).strong());
    let mut action = None;
    for (variant_index, variant) in axis.variants.iter().enumerate() {
        let row_action = ComponentVariantSelectionAction {
            parent_generation,
            catalog_generation,
            axis: WebMediaComponentVariantAxisKind::Audio,
            variant_index,
        };
        let clicked = show_variant_row(
            ui,
            row_action,
            variant_index == axis.active_index,
            audio_label(variant),
            pending_selection,
        );
        if action.is_none() && clicked {
            action = Some(row_action);
        }
    }
    ui.add_space(6.0);
    action
}

/// Рисует provider-owned A/V rendition как одну axis без ложного split-а.
fn show_coupled_axis(
    ui: &mut Ui,
    parent_generation: WebMediaStreamGeneration,
    catalog_generation: ComponentVariantCatalogGeneration,
    axis: &WebMediaCoupledComponentVariantAxis,
    pending_selection: Option<&UrlSidebarPendingSelection>,
) -> Option<ComponentVariantSelectionAction> {
    ui.label(RichText::new(COUPLED_HEADING).strong());
    let mut action = None;
    for (variant_index, variant) in axis.variants.iter().enumerate() {
        let row_action = ComponentVariantSelectionAction {
            parent_generation,
            catalog_generation,
            axis: WebMediaComponentVariantAxisKind::Coupled,
            variant_index,
        };
        let clicked = show_variant_row(
            ui,
            row_action,
            variant_index == axis.active_index,
            coupled_label(variant),
            pending_selection,
        );
        if action.is_none() && clicked {
            action = Some(row_action);
        }
    }
    ui.add_space(6.0);
    action
}

fn show_variant_row(
    ui: &mut Ui,
    row_action: ComponentVariantSelectionAction,
    active: bool,
    label: String,
    pending_selection: Option<&UrlSidebarPendingSelection>,
) -> bool {
    let pending = matches!(
        pending_selection,
        Some(UrlSidebarPendingSelection::Component(pending_action))
            if *pending_action == row_action
    );
    ui.group(|ui| {
        ui.set_min_width(0.0);
        ui.label(if pending {
            "Ожидает переключения"
        } else if active {
            "Активный"
        } else {
            "Доступен"
        });
        ui.label(label);
        variant_button(ui, row_action, active, pending_selection.is_some()).clicked()
    })
    .inner
}

/// Stable widget ID использует только safe catalog generation, axis и row index.
pub(super) fn variant_button(
    ui: &mut Ui,
    row_action: ComponentVariantSelectionAction,
    active: bool,
    switch_in_progress: bool,
) -> Response {
    let response = ui
        .push_id(
            (
                row_action.catalog_generation().value(),
                row_action.axis(),
                row_action.variant_index(),
            ),
            |ui| {
                ui.add_enabled(
                    !active && !switch_in_progress,
                    Button::new(if active {
                        "Активно"
                    } else {
                        "Выбрать"
                    }),
                )
            },
        )
        .inner;
    response.on_disabled_hover_text(if active {
        ACTIVE_VARIANT_TEXT
    } else {
        SWITCH_PENDING_TEXT
    })
}

fn video_label(variant: &WebMediaVideoComponentVariantPresentation) -> String {
    let mut parts = Vec::new();
    match (variant.width, variant.height) {
        (Some(width), Some(height)) => parts.push(format!("{width}×{height}")),
        (None, Some(height)) => parts.push(format!("{height}p")),
        _ => {}
    }
    if let Some((numerator, denominator)) = variant.frame_rate
        && denominator != 0
    {
        parts.push(format!("{:.2} fps", numerator as f64 / denominator as f64));
    }
    if let Some(bitrate) = variant.bitrate {
        parts.push(format!("{} кбит/с", bitrate / 1_000));
    }
    if let Some(codec) = variant.codec {
        parts.push(codec_label(codec).to_owned());
    }
    parts.push(dynamic_range_label(variant.dynamic_range).to_owned());
    fallback_label(parts, "Параметры видео не указаны")
}

fn audio_label(variant: &WebMediaAudioComponentVariantPresentation) -> String {
    let mut parts = Vec::new();
    if let Some(language_label) = &variant.language_label {
        parts.push(language_label.to_string());
    }
    if let Some(bitrate) = variant.bitrate {
        parts.push(format!("{} кбит/с", bitrate / 1_000));
    }
    if let Some(sample_rate_hz) = variant.sample_rate_hz {
        parts.push(format!("{sample_rate_hz} Гц"));
    }
    if let Some(channels) = variant.channels {
        parts.push(format!("{channels} кан."));
    }
    if let Some(codec) = variant.codec {
        parts.push(codec_label(codec).to_owned());
    }
    fallback_label(parts, "Параметры аудио не указаны")
}

/// Объединяет safe metadata одной coupled row, не создавая независимый выбор осей.
fn coupled_label(variant: &WebMediaCoupledComponentVariantPresentation) -> String {
    format!(
        "{} • {}",
        video_label(&variant.video),
        audio_label(&variant.audio)
    )
}

fn fallback_label(parts: Vec<String>, fallback: &str) -> String {
    if parts.is_empty() {
        fallback.to_owned()
    } else {
        parts.join(" • ")
    }
}

fn dynamic_range_label(dynamic_range: DynamicRange) -> &'static str {
    match dynamic_range {
        DynamicRange::Sdr => "SDR",
        DynamicRange::Hdr => "HDR",
        DynamicRange::Unknown => "Dynamic range не указан",
    }
}
