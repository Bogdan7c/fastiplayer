//! Выбор concrete video pipeline для app-owned composition layer-а.
//!
//! Selector остаётся pure: он читает committed config intent и capability snapshot,
//! но не запускает backend, не создаёт WGPU resources и не пересчитывает renderer
//! intersection. Исполнение выбранного плана остаётся в `AppState`.

use capability_core::{SupportedVideoOutput, SystemCapabilities};
use fastiplayer_config::VideoBackendPreference;
use player_core::{PlayerVideoDecoderThreadConfig, VideoDecodeRequirement};
use thiserror::Error;
use video_backend_api::DetachedVideoBackendSelection;
use video_ffmpeg::FFMPEG_SOFTWARE_BACKEND_ID;
use video_frame_contract::{HardwareFrameHandle, VideoFrameTransferPath};

/// Stable backend id из `codec-core`; app layer сравнивает строку без direct dependency.
const VAAPI_BACKEND_ID: &str = "vaapi";

/// Класс concrete backend-а; используется shell-ом, чтобы не пересоздавать pipeline,
/// если нужный backend уже активен.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoBackendKind {
    /// Hardware zero-copy путь (VA-API DMA-BUF сейчас).
    HardwareZeroCopy,

    /// FFmpeg software host-upload путь.
    FfmpegSoftware,
}

/// Concrete video pipeline, который `app-egui` умеет реально запустить сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoPipelinePlan {
    /// Production path: VA-API decode, DMA-BUF zero-copy transfer, WGPU materializer.
    VaapiDmaBufWgpu {
        /// Runtime limits decoder thread-а, которые нужно передать concrete backend factory.
        decoder_thread_config: PlayerVideoDecoderThreadConfig,
    },

    /// Software path: FFmpeg decode, HostPlanar upload, WGPU renderer.
    FfmpegHostUploadWgpu {
        /// Runtime limits decoder thread-а, которые нужно передать concrete backend factory.
        decoder_thread_config: PlayerVideoDecoderThreadConfig,
    },
}

impl VideoPipelinePlan {
    /// Строит concrete app plan только из exact selection, уже сделанного player-ом.
    pub(crate) fn from_player_selection(
        selection: &DetachedVideoBackendSelection,
        decoder_thread_config: PlayerVideoDecoderThreadConfig,
    ) -> Result<Self, PlayerSelectedVideoPipelineError> {
        match (
            selection.expected_backend_id(),
            selection.frame_contract().transfer_path,
        ) {
            (VAAPI_BACKEND_ID, VideoFrameTransferPath::HardwareZeroCopy { .. }) => {
                Ok(Self::VaapiDmaBufWgpu {
                    decoder_thread_config,
                })
            }
            (FFMPEG_SOFTWARE_BACKEND_ID, VideoFrameTransferPath::SoftwareHostUpload) => {
                Ok(Self::FfmpegHostUploadWgpu {
                    decoder_thread_config,
                })
            }
            (backend_id, transfer_path) => Err(
                PlayerSelectedVideoPipelineError::UnsupportedOrMismatchedSelection {
                    backend_id: backend_id.to_owned(),
                    transfer_path,
                },
            ),
        }
    }

    /// Короткая stable метка для startup diagnostics.
    pub const fn diagnostic_label(self) -> &'static str {
        match self {
            Self::VaapiDmaBufWgpu { .. } => "vaapi-dmabuf-wgpu",
            Self::FfmpegHostUploadWgpu { .. } => "ffmpeg-host-upload-wgpu",
        }
    }

    /// Класс backend-а выбранного плана для сравнения с уже активным backend-ом.
    pub const fn backend_kind(self) -> VideoBackendKind {
        match self {
            Self::VaapiDmaBufWgpu { .. } => VideoBackendKind::HardwareZeroCopy,
            Self::FfmpegHostUploadWgpu { .. } => VideoBackendKind::FfmpegSoftware,
        }
    }
}

/// Ошибка mapping-а exact player selection в concrete app composition plan.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum PlayerSelectedVideoPipelineError {
    /// Backend и transfer contract не образуют поддерживаемую production pair.
    #[error("player selection `{backend_id}` несовместим с transfer path {transfer_path:?}")]
    UnsupportedOrMismatchedSelection {
        /// Canonical backend ID из player plan-а.
        backend_id: String,

        /// Exact transfer path того же player plan-а.
        transfer_path: VideoFrameTransferPath,
    },
}

/// Ошибка pure selection до запуска backend-а.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum VideoPipelineSelectionError {
    /// Shell ещё не передал capability snapshot в app state.
    #[error("capability snapshot ещё недоступен для выбора video pipeline")]
    MissingCapabilities,

    /// Запрошен native hardware path, но playable hardware output отсутствует.
    #[error(
        "video.preferred_backend=hardware: нет playable native hardware output после renderer intersection"
    )]
    MissingHardwareOutput,

    /// Native hardware output есть, но он не обслуживает текущий stream requirement.
    #[error(
        "video.preferred_backend=hardware: native hardware output не поддерживает {requirement}; software fallback запрещён preference"
    )]
    HardwareOutputDoesNotServeStream {
        /// Человекочитаемое описание stream requirement из player-core.
        requirement: String,
    },

    /// Запрошен FFmpeg software path, но playable software output/provider пока отсутствует.
    #[error("video.preferred_backend=software: FFmpeg software decode backend сейчас недоступен")]
    MissingSoftwareOutput,

    /// Capability policy нашла playable output, но app composition ещё не умеет стартовать этот backend.
    #[error(
        "video.preferred_backend={preference}: playable backend '{backend_id}' пока не поддержан app composition"
    )]
    UnsupportedCompositionBackend {
        /// Значение public config, из которого пришёл запрос.
        preference: &'static str,
        /// Backend id из renderer-intersected capability output.
        backend_id: String,
    },
}

/// Выбирает concrete plan из committed video config и готового capability snapshot-а.
pub(crate) fn select_video_pipeline_plan(
    preferred_backend: VideoBackendPreference,
    system_capabilities: Option<&SystemCapabilities>,
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
    stream_requirement: Option<&VideoDecodeRequirement>,
) -> Result<VideoPipelinePlan, VideoPipelineSelectionError> {
    let system_capabilities =
        system_capabilities.ok_or(VideoPipelineSelectionError::MissingCapabilities)?;
    let playable_video_outputs = system_capabilities.playable_video_outputs.as_slice();

    match preferred_backend {
        VideoBackendPreference::Auto => select_auto_fallback_plan(
            playable_video_outputs,
            decoder_thread_config,
            stream_requirement,
        ),
        VideoBackendPreference::Hardware
            if playable_video_outputs.iter().any(|output| {
                is_playable_vaapi_dma_buf_output(output)
                    && output_serves_requirement(output, stream_requirement)
            }) =>
        {
            Ok(VideoPipelinePlan::VaapiDmaBufWgpu {
                decoder_thread_config,
            })
        }
        VideoBackendPreference::Hardware => {
            select_hardware_rejection(playable_video_outputs, stream_requirement)
        }
        VideoBackendPreference::Software => select_software_plan(
            VideoBackendPreference::Software,
            playable_video_outputs,
            decoder_thread_config,
            stream_requirement,
        ),
    }
}

/// Выбирает только заранее renderer-intersected software plan для controlled runtime fallback-а.
///
/// В отличие от обычного `auto` selector-а функция намеренно не рассматривает hardware снова:
/// вызывающий код уже получил runtime-only DMA-BUF layout rejection и обязан исключить loop.
pub(crate) fn select_confirmed_software_fallback_plan(
    system_capabilities: Option<&SystemCapabilities>,
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
    stream_requirement: Option<&VideoDecodeRequirement>,
) -> Result<VideoPipelinePlan, VideoPipelineSelectionError> {
    let system_capabilities =
        system_capabilities.ok_or(VideoPipelineSelectionError::MissingCapabilities)?;
    select_software_plan(
        VideoBackendPreference::Auto,
        system_capabilities.playable_video_outputs.as_slice(),
        decoder_thread_config,
        stream_requirement,
    )
}

/// Сообщает, обслуживает ли output текущий стрим (или фильтр выключен при `None`).
fn output_serves_requirement(
    output: &SupportedVideoOutput,
    stream_requirement: Option<&VideoDecodeRequirement>,
) -> bool {
    stream_requirement.is_none_or(|requirement| output.satisfies(requirement))
}

/// Выбирает будущий software fallback только когда playable hardware output вообще отсутствует.
fn select_auto_fallback_plan(
    playable_video_outputs: &[SupportedVideoOutput],
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
    stream_requirement: Option<&VideoDecodeRequirement>,
) -> Result<VideoPipelinePlan, VideoPipelineSelectionError> {
    // Приоритет auto — hardware zero-copy для текущего стрима, затем FFmpeg software.
    if playable_video_outputs.iter().any(|output| {
        is_playable_vaapi_dma_buf_output(output)
            && output_serves_requirement(output, stream_requirement)
    }) {
        return Ok(VideoPipelinePlan::VaapiDmaBufWgpu {
            decoder_thread_config,
        });
    }

    if let Some(native_hardware_output) = playable_video_outputs.iter().find(|output| {
        is_playable_native_hardware_output(output)
            && output_serves_requirement(output, stream_requirement)
    }) {
        return Err(requested_backend_unavailable(
            VideoBackendPreference::Auto,
            native_hardware_output,
        ));
    }

    select_software_plan(
        VideoBackendPreference::Auto,
        playable_video_outputs,
        decoder_thread_config,
        stream_requirement,
    )
}

/// Отклоняет hardware request без перехода на software или raw provider outputs.
fn select_hardware_rejection(
    playable_video_outputs: &[SupportedVideoOutput],
    stream_requirement: Option<&VideoDecodeRequirement>,
) -> Result<VideoPipelinePlan, VideoPipelineSelectionError> {
    let has_native_hardware_output = playable_video_outputs
        .iter()
        .any(is_playable_native_hardware_output);

    if let Some(requirement) = stream_requirement.filter(|_| has_native_hardware_output) {
        let hardware_serves_stream = playable_video_outputs.iter().any(|output| {
            is_playable_native_hardware_output(output)
                && output_serves_requirement(output, Some(requirement))
        });
        if !hardware_serves_stream {
            return Err(
                VideoPipelineSelectionError::HardwareOutputDoesNotServeStream {
                    requirement: requirement.describe(),
                },
            );
        }
    }

    if let Some(native_hardware_output) = playable_video_outputs.iter().find(|output| {
        is_playable_native_hardware_output(output)
            && output_serves_requirement(output, stream_requirement)
    }) {
        return Err(requested_backend_unavailable(
            VideoBackendPreference::Hardware,
            native_hardware_output,
        ));
    }

    Err(VideoPipelineSelectionError::MissingHardwareOutput)
}

/// Software-ветка выбирает только renderer-intersected FFmpeg HostPlanar output.
fn select_software_plan(
    preferred_backend: VideoBackendPreference,
    playable_video_outputs: &[SupportedVideoOutput],
    decoder_thread_config: PlayerVideoDecoderThreadConfig,
    stream_requirement: Option<&VideoDecodeRequirement>,
) -> Result<VideoPipelinePlan, VideoPipelineSelectionError> {
    if playable_video_outputs.iter().any(|output| {
        is_playable_ffmpeg_host_upload_output(output)
            && output_serves_requirement(output, stream_requirement)
    }) {
        return Ok(VideoPipelinePlan::FfmpegHostUploadWgpu {
            decoder_thread_config,
        });
    }

    if let Some(software_output) = playable_video_outputs.iter().find(|output| {
        is_playable_software_output(output) && output_serves_requirement(output, stream_requirement)
    }) {
        return Err(requested_backend_unavailable(
            preferred_backend,
            software_output,
        ));
    }

    Err(VideoPipelineSelectionError::MissingSoftwareOutput)
}

/// Проверяет только уже пересечённый system-level output.
fn is_playable_vaapi_dma_buf_output(output: &SupportedVideoOutput) -> bool {
    output.backend.as_str() == VAAPI_BACKEND_ID && is_playable_native_hardware_output(output)
}

/// Определяет native hardware output независимо от конкретного backend-а, который умеет стартовать app.
fn is_playable_native_hardware_output(output: &SupportedVideoOutput) -> bool {
    output.frame_contract.validate().is_ok()
        && matches!(
            output.frame_contract.transfer_path,
            VideoFrameTransferPath::HardwareZeroCopy {
                handle: HardwareFrameHandle::DmaBuf { .. },
            }
        )
}

/// Определяет software output, который будущий FFmpeg plan сможет исполнить.
fn is_playable_software_output(output: &SupportedVideoOutput) -> bool {
    output.frame_contract.validate().is_ok()
        && matches!(
            output.frame_contract.transfer_path,
            VideoFrameTransferPath::SoftwareHostUpload
        )
}

/// Проверяет software output, который app composition реально умеет стартовать через FFmpeg.
fn is_playable_ffmpeg_host_upload_output(output: &SupportedVideoOutput) -> bool {
    output.backend.as_str() == FFMPEG_SOFTWARE_BACKEND_ID && is_playable_software_output(output)
}

/// Формирует typed ошибку для playable output без доступного app composition plan-а.
fn requested_backend_unavailable(
    preferred_backend: VideoBackendPreference,
    output: &SupportedVideoOutput,
) -> VideoPipelineSelectionError {
    VideoPipelineSelectionError::UnsupportedCompositionBackend {
        preference: preference_label(preferred_backend),
        backend_id: output.backend.as_str().to_string(),
    }
}

/// Возвращает стабильный config label для диагностик selector-а.
fn preference_label(preferred_backend: VideoBackendPreference) -> &'static str {
    match preferred_backend {
        VideoBackendPreference::Auto => "auto",
        VideoBackendPreference::Hardware => "hardware",
        VideoBackendPreference::Software => "software",
    }
}

#[cfg(test)]
mod tests;
