//! Показ кадра на видеослое: исходы по ответам провайдера и по несовпадению пути передачи.
//! Настоящий EGL без окна; пиксели проверяют функциональные тесты рендера.

use gles2_egui::{CreateError, Gles2Context, SurfaceSize};
use gles2_video::{VideoLayerOptions, VideoLayerRenderer};
use video_core::{HostPlaneDescriptor, HostPlaneRole};
use video_frame_contract::{VideoFrameContract, VideoFramePixelLayout};

use super::*;
use crate::host_block_import::HostBlockZeroCopy;
use crate::test_support::fake_decoder::{
    FakeDecoder, LookupMode, ProviderState, SplitPicture, split_picture_frame,
};
use crate::video_import::prepared::FrameSource;

const REQUIRE_EGL_VARIABLE: &str = "GLES2_EGUI_REQUIRE_EGL";

struct Stand {
    context: Gles2Context,
    video: VideoLayerRenderer,
    state: std::sync::Arc<ProviderState>,
    source: FrameSource,
    // Путь без копии с драйвером «не умеет»: эти тесты проверяют копию.
    software_frames: HostBlockZeroCopy,
    // Держит поток декодера (а с ним провайдер) живым.
    _decoder_thread: Box<video_backend_api::VideoBackendDecoderThreadHandle>,
}

fn stand_or_skip(test_name: &str, path: VideoImportPath) -> Option<Stand> {
    let size = SurfaceSize::new(16, 16).expect("non-zero size");
    let context = match Gles2Context::offscreen(size) {
        Ok(context) => context,
        Err(
            error @ (CreateError::EglUnavailable { .. }
            | CreateError::PlatformExtensionMissing { .. }
            | CreateError::DisplayOpen(_)),
        ) => {
            assert!(
                std::env::var_os(REQUIRE_EGL_VARIABLE).is_none(),
                "{test_name}: EGL is required but unavailable: {error}"
            );
            eprintln!("skip {test_name}: no EGL: {error}");
            return None;
        }
        Err(error) => panic!("{test_name}: context creation failed: {error}"),
    };
    let video = VideoLayerRenderer::new(&context, VideoLayerOptions::default()).expect("video");
    let decoder = FakeDecoder::start();
    let state = decoder.state;
    let decoder_thread = decoder.backend.into_decoder_thread();
    let source = FrameSource::new(path, decoder_thread.resource_provider());
    Some(Stand {
        context,
        video,
        state,
        source,
        software_frames: HostBlockZeroCopy::new(crate::capabilities::HardwareZeroCopy::Unavailable),
        _decoder_thread: decoder_thread,
    })
}

fn picture(handle: u64) -> SplitPicture {
    SplitPicture {
        handle,
        size: [16, 16],
        left_luma: 200,
        right_luma: 50,
    }
}

fn stats_of(stand: &Stand) -> gles2_video::VideoLayerStats {
    stand.video.stats()
}

/// Кадр с провайдером «готов» становится текущим (одна загрузка), повтор того же кадра не
/// загружает ничего второй раз.
#[test]
fn a_ready_software_frame_is_uploaded_once_and_a_repeat_is_not() {
    let Some(mut stand) = stand_or_skip(
        "a_ready_software_frame_is_uploaded_once_and_a_repeat_is_not",
        VideoImportPath::SoftwareHostUpload,
    ) else {
        return;
    };
    let (frame, descriptor) = split_picture_frame(picture(7));
    stand.state.add_frame(7, descriptor);

    let first = show_frame_on_layer(
        &mut stand.video,
        &stand.context,
        &mut stand.software_frames,
        &stand.source,
        &frame,
    );
    assert!(
        matches!(first.outcome, LayerShowOutcome::Shown),
        "{first:?}"
    );
    assert_eq!(stats_of(&stand).frames_uploaded, 1);

    let repeat = show_frame_on_layer(
        &mut stand.video,
        &stand.context,
        &mut stand.software_frames,
        &stand.source,
        &frame,
    );
    assert!(
        matches!(repeat.outcome, LayerShowOutcome::Shown),
        "{repeat:?}"
    );
    assert_eq!(
        stats_of(&stand).frames_uploaded,
        1,
        "the current frame is not copied again"
    );
    assert_eq!(stats_of(&stand).repeated_frames, 1);
}

/// Занятость, отсутствие и фатальное состояние провайдера не сливаются, и загрузки не было.
#[test]
fn provider_busy_missing_and_fatal_stay_distinct_and_upload_nothing() {
    let Some(mut stand) = stand_or_skip(
        "provider_busy_missing_and_fatal_stay_distinct_and_upload_nothing",
        VideoImportPath::SoftwareHostUpload,
    ) else {
        return;
    };
    let (frame, descriptor) = split_picture_frame(picture(3));
    stand.state.add_frame(3, descriptor);

    stand.state.set_mode(LookupMode::Busy);
    let busy = show_frame_on_layer(
        &mut stand.video,
        &stand.context,
        &mut stand.software_frames,
        &stand.source,
        &frame,
    );
    assert!(matches!(busy.outcome, LayerShowOutcome::Busy), "{busy:?}");

    stand.state.set_mode(LookupMode::Missing);
    let missing = show_frame_on_layer(
        &mut stand.video,
        &stand.context,
        &mut stand.software_frames,
        &stand.source,
        &frame,
    );
    assert!(
        matches!(missing.outcome, LayerShowOutcome::Missing),
        "{missing:?}"
    );

    stand.state.set_mode(LookupMode::Fatal);
    let fatal = show_frame_on_layer(
        &mut stand.video,
        &stand.context,
        &mut stand.software_frames,
        &stand.source,
        &frame,
    );
    assert!(
        matches!(
            fatal.outcome,
            LayerShowOutcome::Failed(FrameImportError::ProviderFatal)
        ),
        "{fatal:?}"
    );
    assert_eq!(stats_of(&stand).frames_uploaded, 0);
}

/// Кадр другого пути передачи, чем у импорта, — `Unsupported`, и провайдер даже не спрашивают.
#[test]
fn a_frame_of_another_transfer_path_is_unsupported_without_asking_the_provider() {
    for (import_path, contract) in [
        (
            VideoImportPath::HardwareZeroCopy,
            VideoFrameContract::host_yuv420_planar8(),
        ),
        (
            VideoImportPath::SoftwareHostUpload,
            VideoFrameContract::dma_buf_nv12(
                video_frame_contract::DmaBufImageLayout::ComposedLayers,
            ),
        ),
    ] {
        let Some(mut stand) = stand_or_skip(
            "a_frame_of_another_transfer_path_is_unsupported_without_asking_the_provider",
            import_path,
        ) else {
            return;
        };
        let (mut frame, descriptor) = split_picture_frame(picture(9));
        stand.state.add_frame(9, descriptor);
        frame.frame_contract = contract;

        let shown = show_frame_on_layer(
            &mut stand.video,
            &stand.context,
            &mut stand.software_frames,
            &stand.source,
            &frame,
        );

        assert!(
            matches!(
                shown.outcome,
                LayerShowOutcome::Unsupported(
                    FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport
                )
            ),
            "{import_path:?}: {shown:?}"
        );
        assert_eq!(stand.state.descriptor_lookups(), 0, "{import_path:?}");
    }
}

/// Раскладка, которой нет у программной загрузки, — `Unsupported(Layout)`, а дескриптор, который
/// противоречит контракту, — ошибка провайдера, не «не поддерживается».
#[test]
fn unknown_layout_is_unsupported_but_a_broken_descriptor_is_an_error() {
    let Some(mut stand) = stand_or_skip(
        "unknown_layout_is_unsupported_but_a_broken_descriptor_is_an_error",
        VideoImportPath::SoftwareHostUpload,
    ) else {
        return;
    };
    let (mut rgba_frame, descriptor) = split_picture_frame(picture(1));
    stand.state.add_frame(1, descriptor);
    rgba_frame.frame_contract = VideoFrameContract {
        pixel_layout: VideoFramePixelLayout::Rgba8,
        transfer_path: video_frame_contract::VideoFrameTransferPath::SoftwareHostUpload,
    };
    let layout = show_frame_on_layer(
        &mut stand.video,
        &stand.context,
        &mut stand.software_frames,
        &stand.source,
        &rgba_frame,
    );
    assert!(
        matches!(
            layout.outcome,
            LayerShowOutcome::Unsupported(
                FrameImportUnsupportedReason::LayoutNotHandledByActiveImport
            )
        ),
        "{layout:?}"
    );

    // Кадр 16×16 с дескриптором одной плоскости: провайдер нарушил обещание контракта.
    let (frame, _) = split_picture_frame(picture(2));
    let broken = video_core::HostPlanarFrameDescriptor::from_owned_buffer(
        vec![0_u8; 16 * 16],
        vec![HostPlaneDescriptor {
            role: HostPlaneRole::Luma,
            offset: 0,
            stride: 16,
            visible_width: 16,
            visible_height: 16,
            bytes_per_sample: 1,
        }],
    );
    stand.state.add_frame(2, broken);
    let broken = show_frame_on_layer(
        &mut stand.video,
        &stand.context,
        &mut stand.software_frames,
        &stand.source,
        &frame,
    );
    assert!(
        matches!(
            broken.outcome,
            LayerShowOutcome::Failed(FrameImportError::ImportFailed { .. })
        ),
        "{broken:?}"
    );
    assert_eq!(stats_of(&stand).frames_uploaded, 0);
}

/// Таблица отказов разбора программного дескриптора.
#[test]
fn host_planar_errors_map_to_unsupported_or_import_failure() {
    use crate::HostPlanarFrameError as E;
    assert!(matches!(
        host_planar_error_outcome(E::DmaBufDescriptor),
        LayerShowOutcome::Unsupported(
            FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport
        )
    ));
    assert!(matches!(
        host_planar_error_outcome(E::NotSoftwareHostUpload(
            video_frame_contract::VideoFrameTransferPath::SoftwareHostUpload
        )),
        LayerShowOutcome::Unsupported(
            FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport
        )
    ));
    assert!(matches!(
        host_planar_error_outcome(E::UnsupportedLayout(VideoFramePixelLayout::P010)),
        LayerShowOutcome::Unsupported(FrameImportUnsupportedReason::LayoutNotHandledByActiveImport)
    ));
    for error in [
        E::ContractRejected {
            message: "stride".into(),
        },
        E::PlaneUnavailable {
            plane_index: 1,
            message: "gone".into(),
        },
    ] {
        assert!(
            matches!(
                host_planar_error_outcome(error.clone()),
                LayerShowOutcome::Failed(FrameImportError::ImportFailed { .. })
            ),
            "{error:?}"
        );
    }
}
