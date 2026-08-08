use std::sync::Arc;

use egui::{Event, Modifiers, PointerButton, RawInput};
use web_media_core::{CodecFamily, ComponentVariantCatalogGeneration, DynamicRange};

use super::component_variants::{
    AUDIO_AXIS_MISSING_TEXT, AUDIO_HEADING, COUPLED_HEADING, UNAVAILABLE_TEXT, VIDEO_HEADING,
    variant_button,
};
use super::*;
use crate::web_media_stream_model::component_variants::{
    ComponentVariantSelectionAction, WebMediaAudioComponentVariantAxis,
    WebMediaAudioComponentVariantPresentation, WebMediaComponentVariantAxisKind,
    WebMediaComponentVariantProjection, WebMediaCoupledComponentVariantAxis,
    WebMediaCoupledComponentVariantPresentation, WebMediaInstalledComponentVariantPresentation,
    WebMediaVideoComponentVariantAxis, WebMediaVideoComponentVariantPresentation,
};
use crate::web_media_stream_model::{UrlSidebarPendingSelection, WebMediaStreamGeneration};

fn parent_generation() -> WebMediaStreamGeneration {
    WebMediaStreamGeneration::for_test(7, 11)
}

fn unrelated_pending_selection() -> UrlSidebarPendingSelection {
    UrlSidebarPendingSelection::Component(ComponentVariantSelectionAction {
        parent_generation: parent_generation(),
        catalog_generation: ComponentVariantCatalogGeneration::new(99),
        axis: WebMediaComponentVariantAxisKind::Audio,
        variant_index: 8,
    })
}

fn accessible_labels(
    projection: &WebMediaComponentVariantProjection,
    switch_pending: bool,
) -> Vec<String> {
    let pending_selection = switch_pending.then(unrelated_pending_selection);
    accessible_labels_with_pending(projection, pending_selection.as_ref())
}

fn accessible_labels_with_pending(
    projection: &WebMediaComponentVariantProjection,
    pending_selection: Option<&UrlSidebarPendingSelection>,
) -> Vec<String> {
    let context = egui::Context::default();
    context.enable_accesskit();
    let full_output = context.run_ui(egui::RawInput::default(), |ui| {
        let _action =
            component_variants::show(ui, parent_generation(), projection, pending_selection);
    });
    full_output
        .platform_output
        .accesskit_update
        .expect("AccessKit tree update")
        .nodes
        .iter()
        .filter_map(|(_, node)| node.label().or_else(|| node.value()).map(ToOwned::to_owned))
        .collect()
}

fn button_frame(
    context: &egui::Context,
    input: RawInput,
    row_action: ComponentVariantSelectionAction,
    active: bool,
    switch_in_progress: bool,
) -> (egui::Rect, Option<ComponentVariantSelectionAction>, bool) {
    let mut button_rect = egui::Rect::NOTHING;
    let mut action = None;
    let mut enabled = false;
    let _full_output = context.run_ui(input, |ui| {
        let response = variant_button(ui, row_action, active, switch_in_progress);
        button_rect = response.rect;
        enabled = response.enabled();
        action = response.clicked().then_some(row_action);
    });
    (button_rect, action, enabled)
}

fn pointer_button_input(position: egui::Pos2, pressed: bool) -> RawInput {
    RawInput {
        events: vec![Event::PointerButton {
            pos: position,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::default(),
        }],
        ..RawInput::default()
    }
}

#[test]
fn unavailable_projection_keeps_stable_video_and_audio_headings() {
    let labels = accessible_labels(&WebMediaComponentVariantProjection::Unavailable, false);
    assert!(labels.iter().any(|label| label == VIDEO_HEADING));
    assert!(labels.iter().any(|label| label == AUDIO_HEADING));
    assert!(
        labels
            .iter()
            .filter(|label| label.as_str() == UNAVAILABLE_TEXT)
            .count()
            >= 2
    );
}

#[test]
fn installed_missing_axis_is_honest_and_all_buttons_are_disabled() {
    let projection = WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoOnly {
            catalog_generation: ComponentVariantCatalogGeneration::new(4),
            video: WebMediaVideoComponentVariantAxis {
                active_index: 0,
                variants: Arc::from([
                    WebMediaVideoComponentVariantPresentation {
                        width: Some(1280),
                        height: Some(720),
                        frame_rate: Some((60, 1)),
                        bitrate: Some(3_000_000),
                        codec: Some(CodecFamily::Vp9),
                        dynamic_range: DynamicRange::Sdr,
                    },
                    WebMediaVideoComponentVariantPresentation {
                        width: Some(1920),
                        height: Some(1080),
                        frame_rate: Some((60, 1)),
                        bitrate: Some(6_000_000),
                        codec: Some(CodecFamily::Vp9),
                        dynamic_range: DynamicRange::Sdr,
                    },
                ]),
            },
        },
    );
    let labels = accessible_labels(&projection, true);
    assert!(labels.iter().any(|label| label == VIDEO_HEADING));
    assert!(labels.iter().any(|label| label == AUDIO_HEADING));
    assert!(labels.iter().any(|label| label == "Активный"));
    assert!(labels.iter().any(|label| label == AUDIO_AXIS_MISSING_TEXT));

    let context = egui::Context::default();
    let mut responses = Vec::new();
    let mut renderer_result = None;
    let pending_selection = unrelated_pending_selection();
    let _full_output = context.run_ui(egui::RawInput::default(), |ui| {
        let _action = component_variants::show(
            ui,
            parent_generation(),
            &projection,
            Some(&pending_selection),
        );
        renderer_result = Some(());
        for variant_index in 0..2 {
            responses.push(variant_button(
                ui,
                ComponentVariantSelectionAction {
                    parent_generation: parent_generation(),
                    catalog_generation: ComponentVariantCatalogGeneration::new(4),
                    axis: WebMediaComponentVariantAxisKind::Video,
                    variant_index,
                },
                false,
                true,
            ));
        }
    });
    assert_eq!(renderer_result, Some(()));
    assert!(responses.iter().all(|response| !response.enabled()));
    assert!(responses.iter().all(|response| !response.clicked()));
}

#[test]
fn audio_only_installed_projection_reports_missing_video_axis() {
    let projection = WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::AudioOnly {
            catalog_generation: ComponentVariantCatalogGeneration::new(5),
            audio: WebMediaAudioComponentVariantAxis {
                active_index: 0,
                variants: Arc::from([WebMediaAudioComponentVariantPresentation {
                    language_label: Some(Arc::from("uk")),
                    bitrate: Some(128_000),
                    sample_rate_hz: Some(48_000),
                    channels: Some(2),
                    codec: Some(CodecFamily::Opus),
                }]),
            },
        },
    );
    let labels = accessible_labels(&projection, false);
    assert!(labels.iter().any(|label| label == VIDEO_HEADING));
    assert!(labels.iter().any(|label| label == AUDIO_HEADING));
    assert!(
        labels
            .iter()
            .any(|label| label == component_variants::VIDEO_AXIS_MISSING_TEXT)
    );
    assert!(labels.iter().any(|label| label == "Активный"));
    assert!(labels.iter().any(|label| label.contains("uk")));
}

#[test]
fn component_button_ids_are_stable_and_axis_or_index_distinguishes_them() {
    let context = egui::Context::default();
    let render_ids = |context: &egui::Context| {
        let mut ids = Vec::new();
        let _full_output = context.run_ui(egui::RawInput::default(), |ui| {
            for (axis, variant_index) in [
                (WebMediaComponentVariantAxisKind::Video, 0),
                (WebMediaComponentVariantAxisKind::Video, 1),
                (WebMediaComponentVariantAxisKind::Audio, 0),
                (WebMediaComponentVariantAxisKind::Coupled, 0),
            ] {
                ids.push(
                    variant_button(
                        ui,
                        ComponentVariantSelectionAction {
                            parent_generation: parent_generation(),
                            catalog_generation: ComponentVariantCatalogGeneration::new(9),
                            axis,
                            variant_index,
                        },
                        false,
                        false,
                    )
                    .id,
                );
            }
        });
        ids
    };

    let first = render_ids(&context);
    let second = render_ids(&context);
    assert_eq!(first, second);
    assert_ne!(first[0], first[1]);
    assert_ne!(first[0], first[2]);
    assert_ne!(first[0], first[3]);
}

#[test]
fn installed_non_active_row_emits_exact_safe_action_and_active_row_is_disabled() {
    let row_action = ComponentVariantSelectionAction {
        parent_generation: parent_generation(),
        catalog_generation: ComponentVariantCatalogGeneration::new(12),
        axis: WebMediaComponentVariantAxisKind::Video,
        variant_index: 1,
    };
    let context = egui::Context::default();
    let (button_rect, no_action, enabled) =
        button_frame(&context, RawInput::default(), row_action, false, false);
    assert!(enabled);
    assert_eq!(no_action, None);

    let hover_input = RawInput {
        events: vec![Event::PointerMoved(button_rect.center())],
        ..RawInput::default()
    };
    let (_, hover_action, _) = button_frame(&context, hover_input, row_action, false, false);
    let (_, press_action, _) = button_frame(
        &context,
        pointer_button_input(button_rect.center(), true),
        row_action,
        false,
        false,
    );
    let (_, release_action, _) = button_frame(
        &context,
        pointer_button_input(button_rect.center(), false),
        row_action,
        false,
        false,
    );
    assert_eq!(hover_action, None);
    assert_eq!(press_action, None);
    assert_eq!(release_action, Some(row_action));

    let (_, active_action, active_enabled) =
        button_frame(&context, RawInput::default(), row_action, true, false);
    assert!(!active_enabled);
    assert_eq!(active_action, None);
}

#[test]
fn common_pending_disables_component_rows_and_marks_only_exact_component_row() {
    let projection = WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoOnly {
            catalog_generation: ComponentVariantCatalogGeneration::new(13),
            video: WebMediaVideoComponentVariantAxis {
                active_index: 0,
                variants: Arc::from([
                    WebMediaVideoComponentVariantPresentation {
                        width: Some(1280),
                        height: Some(720),
                        frame_rate: None,
                        bitrate: None,
                        codec: Some(CodecFamily::Vp9),
                        dynamic_range: DynamicRange::Sdr,
                    },
                    WebMediaVideoComponentVariantPresentation {
                        width: Some(1920),
                        height: Some(1080),
                        frame_rate: None,
                        bitrate: None,
                        codec: Some(CodecFamily::Vp9),
                        dynamic_range: DynamicRange::Sdr,
                    },
                ]),
            },
        },
    );
    let pending_action = ComponentVariantSelectionAction {
        parent_generation: parent_generation(),
        catalog_generation: ComponentVariantCatalogGeneration::new(13),
        axis: WebMediaComponentVariantAxisKind::Video,
        variant_index: 1,
    };
    let pending_selection = UrlSidebarPendingSelection::Component(pending_action);
    let labels = accessible_labels_with_pending(&projection, Some(&pending_selection));
    assert_eq!(
        labels
            .iter()
            .filter(|label| label.as_str() == "Ожидает переключения")
            .count(),
        2
    );

    let context = egui::Context::default();
    let (_, action, enabled) =
        button_frame(&context, RawInput::default(), pending_action, false, true);
    assert!(!enabled);
    assert_eq!(action, None);
}

#[test]
fn coupled_projection_renders_one_atomic_av_axis_with_safe_metadata() {
    let projection = WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled {
            catalog_generation: ComponentVariantCatalogGeneration::new(14),
            coupled: WebMediaCoupledComponentVariantAxis {
                active_index: 0,
                variants: Arc::from([
                    WebMediaCoupledComponentVariantPresentation {
                        video: WebMediaVideoComponentVariantPresentation {
                            width: Some(1280),
                            height: Some(720),
                            frame_rate: None,
                            bitrate: Some(3_000_000),
                            codec: Some(CodecFamily::H264),
                            dynamic_range: DynamicRange::Sdr,
                        },
                        audio: WebMediaAudioComponentVariantPresentation {
                            language_label: None,
                            bitrate: Some(128_000),
                            sample_rate_hz: Some(48_000),
                            channels: Some(2),
                            codec: Some(CodecFamily::Aac),
                        },
                    },
                    WebMediaCoupledComponentVariantPresentation {
                        video: WebMediaVideoComponentVariantPresentation {
                            width: Some(1920),
                            height: Some(1080),
                            frame_rate: None,
                            bitrate: Some(6_000_000),
                            codec: Some(CodecFamily::H264),
                            dynamic_range: DynamicRange::Sdr,
                        },
                        audio: WebMediaAudioComponentVariantPresentation {
                            language_label: None,
                            bitrate: Some(256_000),
                            sample_rate_hz: Some(48_000),
                            channels: Some(2),
                            codec: Some(CodecFamily::Aac),
                        },
                    },
                ]),
            },
        },
    );

    let labels = accessible_labels(&projection, false);
    assert!(labels.iter().any(|label| label == COUPLED_HEADING));
    assert!(labels.iter().any(|label| label.contains("1280×720")));
    assert!(labels.iter().any(|label| label.contains("128 кбит/с")));
    assert!(labels.iter().all(|label| label != VIDEO_HEADING));
    assert!(labels.iter().all(|label| label != AUDIO_HEADING));
}

#[test]
fn component_renderer_has_no_panel_or_url_sidebar_action_route() {
    let source = include_str!("component_variants.rs");
    assert!(!source.contains("Panel::"));
    assert!(!source.contains("UrlSidebarAction"));
    assert!(source.contains("UrlSidebarPendingSelection"));
    assert!(source.contains("!active && !switch_in_progress"));
}
