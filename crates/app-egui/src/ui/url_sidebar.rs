//! Содержимое URL-секции и typed same-item selection intents.

use egui::{RichText, Ui};
use web_media_core::CodecFamily;

use crate::ui::stream_kind_labels::{LIVE_STREAM_LABEL, VOD_STREAM_LABEL};

use crate::web_media_catalog::{
    WebMediaCatalogState, WebMediaFacetAction, WebMediaFacetOption, WebMediaMode,
};
use crate::web_media_stream_model::component_variants::WebMediaComponentVariantProjection;
use crate::web_media_stream_model::{
    UrlSidebarAction, UrlSidebarItemScope, UrlSidebarModel, UrlSidebarPendingSelection,
    UrlSidebarPlaybackStatus, UrlSidebarSafeError, UrlSidebarSeekAvailability,
    UrlSidebarStreamKind, WebMediaCandidatePresentation, WebMediaSelectionPreference,
    WebMediaStreamGeneration,
};

mod component_variants;
#[cfg(test)]
mod component_variants_tests;

/// Рисует active web-media configuration и возвращает один same-item intent.
pub(crate) fn show(ui: &mut Ui, model: &UrlSidebarModel) -> Option<UrlSidebarAction> {
    crate::ui::egui_behavior::vertical_scroll_area()
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
                    ingress,
                    source_label,
                    status,
                    catalog,
                } => {
                    show_single_web_media(ui, *ingress, source_label, *status, catalog);
                    None
                }
                UrlSidebarModel::CatalogBacked {
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
                    catalog,
                    fallback_notice,
                } => show_catalog_backed(
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
                    catalog,
                    *fallback_notice,
                ),
            }
        })
        .inner
}

fn show_inactive(ui: &mut Ui) {
    ui.heading("Stream");
    ui.label("Local media is active, or the source has not been installed yet.");
    ui.add_space(8.0);
    ui.weak("Use Add URL in Playlist to add a URL.");
}

fn show_single_web_media(
    ui: &mut Ui,
    ingress: web_media_core::WebMediaIngressKind,
    source_label: &str,
    status: UrlSidebarPlaybackStatus,
    catalog: &WebMediaCatalogState,
) {
    let heading = match ingress {
        web_media_core::WebMediaIngressKind::DirectResource => "Direct media file link",
        web_media_core::WebMediaIngressKind::NativeManifest => "Native web manifest",
        web_media_core::WebMediaIngressKind::ExtractorBacked => "Web media",
    };
    ui.heading(heading);
    wrapped_value(ui, "Source", source_label);
    ui.add_space(8.0);
    status_grid(ui, status);
    ui.add_space(8.0);
    let _ = show_stream_picker(ui, None, catalog, false);
    ui.weak("The source has one installed variant; no switch is needed.");
}

#[allow(clippy::too_many_arguments)]
fn show_catalog_backed(
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
    catalog: &WebMediaCatalogState,
    fallback_notice: bool,
) -> Option<UrlSidebarAction> {
    ui.heading("Web media");
    wrapped_value(ui, "Source", source_label);
    wrapped_value(ui, "Application", item_scope_label(item_scope));
    wrapped_value(ui, "Preference", &preference_label(preference));
    ui.add_space(8.0);
    status_grid(ui, status);

    if let Some(error) = safe_error {
        ui.add_space(8.0);
        ui.colored_label(ui.visuals().error_fg_color, safe_error_label(error));
        ui.weak("The source will be reopened; the queue will stay unchanged.");
    }
    if fallback_notice {
        ui.add_space(8.0);
        ui.weak("The saved variant is unavailable; the best available stream is installed.");
    }

    let _legacy_candidate_projection = (candidates, active_candidate);
    ui.add_space(12.0);
    let unified_action =
        show_stream_picker(ui, Some(generation), catalog, pending_selection.is_some());
    let component_action =
        component_variants::show(ui, generation, component_variants, pending_selection);
    choose_single_sidebar_action(unified_action, component_action)
}

fn show_stream_picker(
    ui: &mut Ui,
    parent_generation: Option<WebMediaStreamGeneration>,
    catalog: &WebMediaCatalogState,
    pending: bool,
) -> Option<UrlSidebarAction> {
    match catalog {
        WebMediaCatalogState::Inactive => {
            ui.label(RichText::new("Variants are unavailable.").strong());
            None
        }
        WebMediaCatalogState::Failed { .. } => {
            ui.colored_label(
                ui.visuals().error_fg_color,
                "Could not prepare available variants.",
            );
            None
        }
        WebMediaCatalogState::Ready(catalog) => {
            let projection = catalog.picker_projection();
            let mut action = None;
            ui.add_enabled_ui(!pending, |ui| {
                for selector in projection.selectors.iter() {
                    let selected_label = selector
                        .selected_index
                        .and_then(|index| selector.options.get(index))
                        .map(facet_option_label)
                        .unwrap_or_else(|| "Automatic".to_owned());
                    egui::ComboBox::from_id_salt(("web_media_facet", selector.facet))
                        .selected_text(selected_label)
                        .width(ui.available_width())
                        .show_ui(ui, |ui| {
                            for (option_index, option) in selector.options.iter().enumerate() {
                                let selected = selector.selected_index == Some(option_index);
                                if ui
                                    .selectable_label(selected, facet_option_label(option))
                                    .clicked()
                                    && !selected
                                    && action.is_none()
                                    && let Some(parent_generation) = parent_generation
                                {
                                    action = Some(UrlSidebarAction::StreamFacet {
                                        parent_generation,
                                        action: WebMediaFacetAction {
                                            generation: projection.generation,
                                            facet: selector.facet,
                                            option_index,
                                        },
                                    });
                                }
                            }
                        });
                }
            });
            if pending {
                ui.weak("Switching stream...");
            }
            action
        }
    }
}

fn facet_option_label(option: &WebMediaFacetOption) -> String {
    match option {
        WebMediaFacetOption::Mode(WebMediaMode::Automatic) => "Determined by stream".to_owned(),
        WebMediaFacetOption::Mode(WebMediaMode::VideoAndAudio) => "Video + audio".to_owned(),
        WebMediaFacetOption::Mode(WebMediaMode::VideoOnly) => "Video only".to_owned(),
        WebMediaFacetOption::Mode(WebMediaMode::AudioOnly) => "Audio only".to_owned(),
        WebMediaFacetOption::Codec(codec) => codec_label(*codec).to_owned(),
        WebMediaFacetOption::Resolution { width, height } => {
            format!("{width}x{height} ({height}p)")
        }
        WebMediaFacetOption::FrameRate(rate) => frame_rate_label(*rate),
        WebMediaFacetOption::DynamicRange(web_media_core::DynamicRange::Hdr) => "HDR".to_owned(),
        WebMediaFacetOption::DynamicRange(web_media_core::DynamicRange::Sdr) => "SDR".to_owned(),
        WebMediaFacetOption::DynamicRange(web_media_core::DynamicRange::Unknown)
        | WebMediaFacetOption::Automatic => "Automatic".to_owned(),
    }
}

fn frame_rate_label(rate: web_media_core::FrameRate) -> String {
    if rate.denominator() == 1 {
        return rate.numerator().to_string();
    }
    let mut label = format!(
        "{:.3}",
        f64::from(rate.numerator()) / f64::from(rate.denominator())
    );
    while label.ends_with('0') {
        label.pop();
    }
    label
}

/// Один frame публикует не более одного intent; unified selector имеет приоритет.
fn choose_single_sidebar_action(
    unified_action: Option<UrlSidebarAction>,
    component_action: Option<
        crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction,
    >,
) -> Option<UrlSidebarAction> {
    unified_action.or(component_action.map(UrlSidebarAction::ComponentVariant))
}

/// Русская подпись режима потока; неизвестный режим не маскируется под VOD.
fn stream_kind_label(kind: UrlSidebarStreamKind) -> &'static str {
    match kind {
        UrlSidebarStreamKind::Live => LIVE_STREAM_LABEL,
        UrlSidebarStreamKind::Vod => VOD_STREAM_LABEL,
        UrlSidebarStreamKind::Unknown => "Detecting",
    }
}

fn seek_availability_label(seek: UrlSidebarSeekAvailability) -> &'static str {
    match seek {
        UrlSidebarSeekAvailability::Unknown => "Detecting",
        UrlSidebarSeekAvailability::Unavailable => "Unavailable",
        UrlSidebarSeekAvailability::Available => "Available",
        UrlSidebarSeekAvailability::WithinLiveWindow => "Within the live window",
    }
}

fn status_grid(ui: &mut Ui, status: UrlSidebarPlaybackStatus) {
    egui::Grid::new("url_stream_status_grid")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.weak("Mode");
            ui.label(stream_kind_label(status.stream_kind));
            ui.end_row();

            ui.weak("Seeking");
            ui.label(seek_availability_label(status.seek));
            ui.end_row();

            ui.weak("Buffering");
            ui.label(if status.buffering {
                "In progress"
            } else {
                "Not required"
            });
            ui.end_row();

            ui.weak("Refresh");
            ui.label(if status.refresh_on_reopen {
                "When reopened"
            } else {
                "Not required"
            });
            ui.end_row();
        });
}

fn wrapped_value(ui: &mut Ui, label: &str, value: &str) {
    ui.horizontal_wrapped(|ui| {
        ui.weak(format!("{label}:"));
        ui.label(value);
    });
}

fn preference_label(preference: WebMediaSelectionPreference) -> String {
    match preference {
        WebMediaSelectionPreference::GlobalBestPlayable => {
            "Global: automatic (fast start)".to_owned()
        }
        WebMediaSelectionPreference::GlobalPreferredHeight(height) => {
            format!("Global: prefer {height}p")
        }
        WebMediaSelectionPreference::ItemOverride(None) => {
            "For this item: best available".to_owned()
        }
        WebMediaSelectionPreference::ItemOverride(Some(height)) => {
            format!("For this item: {height}p")
        }
    }
}

fn item_scope_label(scope: UrlSidebarItemScope) -> &'static str {
    match scope {
        UrlSidebarItemScope::Detached => "Outside the current queue",
        UrlSidebarItemScope::SingleItem => "Current Playlist item",
        UrlSidebarItemScope::CompoundPart => "Current part of a compound item",
    }
}

fn safe_error_label(error: UrlSidebarSafeError) -> &'static str {
    match error {
        UrlSidebarSafeError::SourceUnavailable => "The web source is temporarily unavailable.",
        UrlSidebarSafeError::SameItemSwitchBusy => "A switch is already in progress.",
        UrlSidebarSafeError::SameItemSwitchStale => {
            "The variant list is outdated; select the desired row again."
        }
        UrlSidebarSafeError::SameItemSwitchCancelled => "Switch canceled.",
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
    use std::sync::Arc;

    use web_media_core::{CodecFamily, ContainerFamily, DynamicRange, StreamLayoutKind};

    use crate::web_media_stream_model::component_variants::{
        WebMediaComponentVariantProjection, WebMediaInstalledComponentVariantPresentation,
        WebMediaVideoComponentVariantAxis, WebMediaVideoComponentVariantPresentation,
    };
    use crate::web_media_stream_model::{
        UrlSidebarItemScope, UrlSidebarModel, UrlSidebarPlaybackStatus, UrlSidebarSeekAvailability,
        UrlSidebarStreamKind, WebMediaCandidatePresentation, WebMediaContainerSummary,
        WebMediaSelectionPreference, WebMediaStreamGeneration,
    };

    fn candidate_presentation() -> WebMediaCandidatePresentation {
        WebMediaCandidatePresentation {
            layout: StreamLayoutKind::Muxed,
            width: Some(1920),
            height: Some(1080),
            frame_rate: Some((30, 1)),
            video_bitrate: Some(4_000_000),
            audio_bitrate: Some(128_000),
            video_codec: Some(CodecFamily::H264),
            audio_codec: Some(CodecFamily::Aac),
            dynamic_range: Some(DynamicRange::Sdr),
            containers: WebMediaContainerSummary {
                video: Some(ContainerFamily::MpegTs),
                audio: Some(ContainerFamily::MpegTs),
            },
        }
    }

    fn visible_labels(model: &UrlSidebarModel) -> Vec<String> {
        let context = egui::Context::default();
        context.enable_accesskit();
        let output =
            crate::ui::test_frame::run_ui_frame(&context, egui::RawInput::default(), |ui| {
                let _action = super::show(ui, model);
            });
        output
            .platform_output
            .accesskit_update
            .expect("AccessKit tree update")
            .nodes
            .iter()
            .filter_map(|(_, node)| node.label().or_else(|| node.value()).map(ToOwned::to_owned))
            .collect()
    }

    fn direct_model_with(stream_kind: UrlSidebarStreamKind) -> UrlSidebarModel {
        direct_model_with_seek(stream_kind, UrlSidebarSeekAvailability::Unavailable)
    }

    fn direct_model_with_seek(
        stream_kind: UrlSidebarStreamKind,
        seek: UrlSidebarSeekAvailability,
    ) -> UrlSidebarModel {
        UrlSidebarModel::DirectMedia {
            ingress: web_media_core::WebMediaIngressKind::DirectResource,
            source_label: Arc::from("media.example.test"),
            status: UrlSidebarPlaybackStatus {
                stream_kind,
                seek,
                buffering: false,
                refresh_on_reopen: false,
            },
            catalog: crate::web_media_catalog::installed_only_catalog_state_for_test(),
        }
    }

    /// Строка «Режим» показывает ровно ту подпись, что соответствует виду потока.
    #[test]
    fn mode_row_names_live_vod_and_unknown_streams_honestly() {
        let live = visible_labels(&direct_model_with(UrlSidebarStreamKind::Live));
        assert!(live.iter().any(|label| label == "Live stream"));
        assert!(!live.iter().any(|label| label == "VOD"));

        let vod = visible_labels(&direct_model_with(UrlSidebarStreamKind::Vod));
        assert!(vod.iter().any(|label| label == "VOD"));
        assert!(!vod.iter().any(|label| label == "Live stream"));

        let unknown = visible_labels(&direct_model_with(UrlSidebarStreamKind::Unknown));
        assert!(unknown.iter().any(|label| label == "Detecting"));
        assert!(!unknown.iter().any(|label| label == "VOD"));
    }

    /// A14: строка «Перемотка» показывает то, что сообщил player, включая «в пределах окна».
    #[test]
    fn seek_row_names_each_availability_honestly() {
        let cases = [
            (UrlSidebarSeekAvailability::Unknown, "Detecting"),
            (UrlSidebarSeekAvailability::Unavailable, "Unavailable"),
            (UrlSidebarSeekAvailability::Available, "Available"),
            (
                UrlSidebarSeekAvailability::WithinLiveWindow,
                "Within the live window",
            ),
        ];
        for (seek, expected) in cases {
            let labels = visible_labels(&direct_model_with_seek(UrlSidebarStreamKind::Live, seek));
            assert!(labels.iter().any(|label| label == expected), "{labels:?}");
            for (_, other) in cases.iter().filter(|(_, text)| *text != expected) {
                // «Определяется» легально встречается и в строке «Режим» только для Unknown-режима.
                assert!(
                    !labels.iter().any(|label| label == other),
                    "{other}: {labels:?}"
                );
            }
        }
    }

    /// A14: заголовок direct-ссылки не называет её «web-потоком/эфиром»; для VOD не звучит как live.
    #[test]
    fn direct_resource_heading_names_a_file_link_not_a_live_stream() {
        let labels = visible_labels(&direct_model_with(UrlSidebarStreamKind::Vod));
        assert!(labels.iter().any(|label| label == "Direct media file link"));
        assert!(!labels.iter().any(|label| label.contains("web stream")));
    }

    /// A10: под ошибкой источника нет технического жаргона («controlled reopen»).
    #[test]
    fn source_error_hint_is_plain_english_without_internal_jargon() {
        let active_candidate = candidate_presentation();
        let model = UrlSidebarModel::CatalogBacked {
            generation: WebMediaStreamGeneration::for_test(1, 1),
            source_label: Arc::from("source"),
            candidates: Arc::from([active_candidate.clone()]),
            active_candidate,
            pending_selection: None,
            component_variants: Box::new(WebMediaComponentVariantProjection::Unavailable),
            preference: WebMediaSelectionPreference::GlobalBestPlayable,
            item_scope: UrlSidebarItemScope::SingleItem,
            status: UrlSidebarPlaybackStatus {
                stream_kind: UrlSidebarStreamKind::Vod,
                seek: UrlSidebarSeekAvailability::Available,
                buffering: false,
                refresh_on_reopen: false,
            },
            safe_error: Some(crate::web_media_stream_model::UrlSidebarSafeError::SourceUnavailable),
            catalog: crate::web_media_catalog::WebMediaCatalogState::Inactive,
            fallback_notice: false,
        };

        let labels = visible_labels(&model);

        assert!(
            labels
                .iter()
                .any(|label| label == "The web source is temporarily unavailable.")
        );
        assert!(
            labels
                .iter()
                .any(|label| label.contains("The source will be reopened"))
        );
        assert!(
            !labels
                .iter()
                .any(|label| label.contains("controlled reopen"))
        );
    }

    #[test]
    fn url_content_does_not_create_second_sidebar_panel() {
        let source = include_str!("url_sidebar.rs");
        let panel_constructor_prefix = format!("{}{}", "Panel", "::");
        assert_eq!(source.matches(&panel_constructor_prefix).count(), 0);
    }

    #[test]
    fn production_url_model_renders_component_axis_after_unified_catalog() {
        let generation = WebMediaStreamGeneration::for_test(4, 9);
        let active_candidate = candidate_presentation();
        let model = UrlSidebarModel::CatalogBacked {
            generation,
            source_label: Arc::from("acceptance source"),
            candidates: Arc::from([active_candidate.clone()]),
            active_candidate,
            pending_selection: None,
            component_variants: Box::new(WebMediaComponentVariantProjection::Installed(
                WebMediaInstalledComponentVariantPresentation::VideoOnly {
                    catalog_generation: web_media_core::ComponentVariantCatalogGeneration::new(3),
                    video: WebMediaVideoComponentVariantAxis {
                        active_index: 0,
                        variants: Arc::from([WebMediaVideoComponentVariantPresentation {
                            width: Some(1920),
                            height: Some(1080),
                            frame_rate: Some((30, 1)),
                            bitrate: Some(4_000_000),
                            codec: Some(CodecFamily::H264),
                            dynamic_range: DynamicRange::Sdr,
                        }]),
                    },
                },
            )),
            preference: WebMediaSelectionPreference::GlobalBestPlayable,
            item_scope: UrlSidebarItemScope::SingleItem,
            status: UrlSidebarPlaybackStatus {
                stream_kind: UrlSidebarStreamKind::Vod,
                seek: UrlSidebarSeekAvailability::Available,
                buffering: false,
                refresh_on_reopen: false,
            },
            safe_error: None,
            catalog: crate::web_media_catalog::WebMediaCatalogState::Inactive,
            fallback_notice: false,
        };

        let labels = visible_labels(&model);
        assert!(labels.iter().any(|label| label == "Video"));
        assert!(labels.iter().any(|label| label.contains("1920×1080")));
        assert!(labels.iter().any(|label| label == "Active"));
    }

    #[test]
    fn direct_single_option_is_visible_but_cannot_emit_selection_action() {
        let model = UrlSidebarModel::DirectMedia {
            ingress: web_media_core::WebMediaIngressKind::DirectResource,
            source_label: Arc::from("media.example.test"),
            status: UrlSidebarPlaybackStatus {
                stream_kind: UrlSidebarStreamKind::Vod,
                seek: UrlSidebarSeekAvailability::Available,
                buffering: false,
                refresh_on_reopen: false,
            },
            catalog: crate::web_media_catalog::installed_only_catalog_state_for_test(),
        };
        let context = egui::Context::default();
        let mut action = None;
        let _output =
            crate::ui::test_frame::run_ui_frame(&context, egui::RawInput::default(), |ui| {
                action = super::show(ui, &model);
            });

        assert!(action.is_none());
        let labels = visible_labels(&model);
        assert!(
            labels
                .iter()
                .any(|label| label.contains("one installed variant"))
        );
    }

    #[test]
    fn primary_action_wins_if_both_routes_click_in_same_frame() {
        let generation = crate::web_media_stream_model::WebMediaStreamGeneration::for_test(3, 5);
        let primary_action = crate::web_media_stream_model::UrlSidebarAction::Candidate {
            generation,
            candidate_index: 2,
        };
        let component_action =
            crate::web_media_stream_model::component_variants::ComponentVariantSelectionAction {
                parent_generation: generation,
                catalog_generation: web_media_core::ComponentVariantCatalogGeneration::new(8),
                axis: crate::web_media_stream_model::component_variants::WebMediaComponentVariantAxisKind::Audio,
                variant_index: 1,
            };

        assert_eq!(
            super::choose_single_sidebar_action(Some(primary_action), Some(component_action),),
            Some(primary_action)
        );
    }
}
