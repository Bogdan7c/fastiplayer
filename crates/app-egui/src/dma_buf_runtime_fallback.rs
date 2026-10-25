//! Controlled runtime fallback policy для DMA-BUF layout, известного только после first frame.

use fastiplayer_config::VideoBackendPreference;
use render_shell_api::{FrameImportUnsupportedReason, HardwareFrameRefusal};
use video_core::DmaBufDescriptorRejection;

use crate::state::VideoPipelineRebuildError;
use crate::video_pipeline_selector::{VideoPipelinePlan, VideoPipelineSelectionError};

/// Причина, по которой аппаратный кадр нельзя показать и допустим программный фолбэк.
///
/// Два разных источника, не слитых в один: отказ дескриптора (его нашёл валидатор по данным
/// декодера) и отказ самого рендера (решение владельца 41: рендер не умеет показывать кадр).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DmaBufFallbackTrigger {
    /// Дескриптор DMA-BUF отклонён до импорта.
    DescriptorRejected(DmaBufDescriptorRejection),
    /// Рендер не может показать аппаратный кадр с исправным дескриптором.
    RendererCannotShowHardwareFrame(HardwareFrameRefusal),
}

impl DmaBufFallbackTrigger {
    /// Выделяет причину фолбэка из отказа склада кадров; `None` — отказ не про аппаратный
    /// кадр (контракт не объявлен, не тот путь передачи): фолбэк тут не лечит.
    pub(crate) fn from_unsupported_reason(reason: &FrameImportUnsupportedReason) -> Option<Self> {
        match reason {
            FrameImportUnsupportedReason::DmaBufDescriptorRejected(rejection) => {
                Some(Self::DescriptorRejected(rejection.clone()))
            }
            FrameImportUnsupportedReason::RendererCannotShowHardwareFrame(refusal) => {
                Some(Self::RendererCannotShowHardwareFrame(refusal.clone()))
            }
            FrameImportUnsupportedReason::ContractNotDeclared { .. }
            | FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport
            | FrameImportUnsupportedReason::LayoutNotHandledByActiveImport => None,
        }
    }

    /// Лечится ли причина переходом на программный декод.
    ///
    /// Отказ рендера лечится всегда; из отказов дескриптора — только составной
    /// multi-object (прежняя политика приложения, остальные остаются ошибкой).
    pub(crate) const fn is_cured_by_software_decode(&self) -> bool {
        matches!(
            self,
            Self::RendererCannotShowHardwareFrame(_)
                | Self::DescriptorRejected(
                    DmaBufDescriptorRejection::UnsupportedComposedMultiObject { .. }
                )
        )
    }
}

impl std::fmt::Display for DmaBufFallbackTrigger {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::DescriptorRejected(rejection) => write!(formatter, "{rejection}"),
            Self::RendererCannotShowHardwareFrame(refusal) => write!(formatter, "{refusal}"),
        }
    }
}

/// Pending runtime rejection, перенесённая из materialization stage к composition boundary.
#[derive(Debug)]
pub(crate) struct PendingDmaBufFallback {
    /// Исходная причина, по которой аппаратный кадр нельзя показать.
    pub(crate) trigger: DmaBufFallbackTrigger,
    /// Generation кадра, на котором обнаружено ограничение.
    pub(crate) render_generation: u64,
    /// Typed player error с frame identity для финальной диагностики.
    pub(crate) player_error: player_core::PlayerRenderError,
}

/// Редкий error-path payload для shell reporting без раздувания success-path `Result`.
#[derive(Debug)]
pub(crate) struct DmaBufRuntimeFallbackFailure {
    /// Typed recovery policy/startup/restore error.
    pub(crate) error: DmaBufRuntimeFallbackError,
    /// Player-facing typed error с identity исходного frame lease-а.
    pub(crate) player_error: player_core::PlayerRenderError,
}

/// Typed итог отказа runtime layout recovery.
#[derive(Debug)]
pub(crate) enum DmaBufRuntimeFallbackError {
    /// `hardware` запрещает software fallback по явной user policy.
    HardwareFallbackForbidden {
        /// Исходная причина, по которой аппаратный кадр нельзя показать.
        trigger: DmaBufFallbackTrigger,
    },
    /// Текущий backend не является `auto` hardware path-ом, поэтому fallback неприменим.
    FallbackNotApplicable {
        /// Исходная причина, по которой аппаратный кадр нельзя показать.
        trigger: DmaBufFallbackTrigger,
    },
    /// Для этого media generation controlled fallback уже пытались выполнить.
    FallbackLoopPrevented {
        /// Исходная причина, по которой аппаратный кадр нельзя показать.
        trigger: DmaBufFallbackTrigger,
        /// Render generation, к которому привязана единственная попытка.
        render_generation: u64,
    },
    /// Capability snapshot не содержит заранее подтверждённого playable software plan-а.
    SoftwarePipelineUnavailable {
        /// Исходная причина, по которой аппаратный кадр нельзя показать.
        trigger: DmaBufFallbackTrigger,
        /// Typed причина отказа selector-а.
        selection_error: VideoPipelineSelectionError,
    },
    /// Software backend startup либо worker restore/commit завершились ошибкой.
    SoftwarePipelineFailed {
        /// Исходная причина, по которой аппаратный кадр нельзя показать.
        trigger: DmaBufFallbackTrigger,
        /// Typed failure controlled pipeline rebuild-а.
        fallback_failure: VideoPipelineRebuildError,
    },
}

impl std::fmt::Display for DmaBufRuntimeFallbackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HardwareFallbackForbidden { trigger } => write!(
                formatter,
                "hardware frame cannot be shown: {trigger}; software fallback is forbidden by video.preferred_backend=hardware"
            ),
            Self::FallbackNotApplicable { trigger } => write!(
                formatter,
                "hardware frame cannot be shown: {trigger}; runtime software fallback is not applicable to the active preference"
            ),
            Self::FallbackLoopPrevented {
                trigger,
                render_generation,
            } => write!(
                formatter,
                "hardware frame cannot be shown: {trigger}; fallback loop prevented for render generation {render_generation}"
            ),
            Self::SoftwarePipelineUnavailable {
                trigger,
                selection_error,
            } => write!(
                formatter,
                "hardware frame cannot be shown: {trigger}; confirmed software fallback is unavailable: {selection_error}"
            ),
            Self::SoftwarePipelineFailed {
                trigger,
                fallback_failure,
            } => write!(
                formatter,
                "hardware frame cannot be shown: {trigger}; controlled software fallback failed: {fallback_failure}"
            ),
        }
    }
}

impl std::error::Error for DmaBufRuntimeFallbackError {}

/// Хранит exactly-once guard для runtime fallback-а текущего media generation.
#[derive(Debug, Default)]
pub(crate) struct DmaBufRuntimeFallbackController {
    attempted_render_generation: Option<u64>,
}

impl DmaBufRuntimeFallbackController {
    /// Применяет user policy и возвращает только заранее подтверждённый software plan.
    pub(crate) fn begin(
        &mut self,
        preference: VideoBackendPreference,
        render_generation: u64,
        trigger: DmaBufFallbackTrigger,
        software_plan: Result<VideoPipelinePlan, VideoPipelineSelectionError>,
    ) -> Result<VideoPipelinePlan, DmaBufRuntimeFallbackError> {
        match preference {
            VideoBackendPreference::Hardware => {
                return Err(DmaBufRuntimeFallbackError::HardwareFallbackForbidden { trigger });
            }
            VideoBackendPreference::Software => {
                return Err(DmaBufRuntimeFallbackError::FallbackNotApplicable { trigger });
            }
            VideoBackendPreference::Auto => {}
        }

        if self.attempted_render_generation.is_some() {
            return Err(DmaBufRuntimeFallbackError::FallbackLoopPrevented {
                trigger,
                render_generation,
            });
        }
        // Guard фиксируется до selection/startup: unavailable и failed попытки тоже не
        // должны повторяться на каждом redraw и образовывать recovery loop.
        self.attempted_render_generation = Some(render_generation);

        software_plan.map_err(|selection_error| {
            DmaBufRuntimeFallbackError::SoftwarePipelineUnavailable {
                trigger,
                selection_error,
            }
        })
    }

    /// Открытие нового media создаёт новый recovery scope.
    pub(crate) fn reset_for_new_media(&mut self) {
        self.attempted_render_generation = None;
    }

    /// Оборачивает startup/restore failure, не теряя исходную typed layout rejection.
    pub(crate) fn fallback_failed(
        trigger: DmaBufFallbackTrigger,
        fallback_failure: VideoPipelineRebuildError,
    ) -> DmaBufRuntimeFallbackError {
        DmaBufRuntimeFallbackError::SoftwarePipelineFailed {
            trigger,
            fallback_failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use player_core::PlayerVideoDecoderThreadConfig;

    use super::*;

    fn unsupported_layout() -> DmaBufFallbackTrigger {
        DmaBufFallbackTrigger::DescriptorRejected(
            DmaBufDescriptorRejection::UnsupportedComposedMultiObject {
                first_object_index: 0,
                conflicting_object_index: 1,
            },
        )
    }

    fn software_plan() -> VideoPipelinePlan {
        VideoPipelinePlan::FfmpegHostUpload {
            decoder_thread_config: PlayerVideoDecoderThreadConfig::default(),
        }
    }

    #[test]
    fn auto_accepts_one_confirmed_software_fallback() {
        let mut controller = DmaBufRuntimeFallbackController::default();

        let selected = controller
            .begin(
                VideoBackendPreference::Auto,
                7,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect("confirmed software fallback must be selected");

        assert_eq!(selected, software_plan());
    }

    #[test]
    fn auto_preserves_layout_error_with_unavailable_software_context() {
        let mut controller = DmaBufRuntimeFallbackController::default();

        let error = controller
            .begin(
                VideoBackendPreference::Auto,
                7,
                unsupported_layout(),
                Err(VideoPipelineSelectionError::MissingSoftwareOutput),
            )
            .expect_err("unavailable software path must reject fallback");

        assert!(matches!(
            error,
            DmaBufRuntimeFallbackError::SoftwarePipelineUnavailable {
                trigger: DmaBufFallbackTrigger::DescriptorRejected(
                    DmaBufDescriptorRejection::UnsupportedComposedMultiObject { .. }
                ),
                selection_error: VideoPipelineSelectionError::MissingSoftwareOutput,
            }
        ));
    }

    #[test]
    fn auto_preserves_layout_error_with_failed_startup_or_restore_context() {
        let error = DmaBufRuntimeFallbackController::fallback_failed(
            unsupported_layout(),
            VideoPipelineRebuildError::BackendStartup {
                plan_label: "ffmpeg-host-upload-wgpu",
                message: "software startup failed".to_string(),
            },
        );

        assert!(matches!(
            error,
            DmaBufRuntimeFallbackError::SoftwarePipelineFailed {
                trigger: DmaBufFallbackTrigger::DescriptorRejected(
                    DmaBufDescriptorRejection::UnsupportedComposedMultiObject { .. }
                ),
                fallback_failure: VideoPipelineRebuildError::BackendStartup { .. },
            }
        ));
    }

    #[test]
    fn hardware_never_falls_back_to_software() {
        let mut controller = DmaBufRuntimeFallbackController::default();

        let error = controller
            .begin(
                VideoBackendPreference::Hardware,
                7,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect_err("hardware preference must reject software fallback");

        assert!(matches!(
            error,
            DmaBufRuntimeFallbackError::HardwareFallbackForbidden { .. }
        ));
    }

    #[test]
    fn auto_prevents_fallback_loop_for_same_generation() {
        let mut controller = DmaBufRuntimeFallbackController::default();
        controller
            .begin(
                VideoBackendPreference::Auto,
                7,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect("first attempt must be allowed");

        let error = controller
            .begin(
                VideoBackendPreference::Auto,
                7,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect_err("second attempt in same generation must be blocked");

        assert!(matches!(
            error,
            DmaBufRuntimeFallbackError::FallbackLoopPrevented {
                render_generation: 7,
                ..
            }
        ));
    }

    #[test]
    fn render_generation_change_alone_does_not_reopen_fallback_loop() {
        let mut controller = DmaBufRuntimeFallbackController::default();
        controller
            .begin(
                VideoBackendPreference::Auto,
                7,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect("first media may fallback");

        let error = controller
            .begin(
                VideoBackendPreference::Auto,
                8,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect_err("backend rollback generation changes must not reopen fallback");
        assert!(matches!(
            error,
            DmaBufRuntimeFallbackError::FallbackLoopPrevented { .. }
        ));
    }

    #[test]
    fn explicit_new_media_boundary_reopens_one_shot_fallback() {
        let mut controller = DmaBufRuntimeFallbackController::default();
        controller
            .begin(
                VideoBackendPreference::Auto,
                7,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect("first media may fallback");

        controller.reset_for_new_media();
        controller
            .begin(
                VideoBackendPreference::Auto,
                8,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect("explicit new media boundary receives a fresh one-shot guard");
    }

    fn renderer_refusal() -> DmaBufFallbackTrigger {
        DmaBufFallbackTrigger::RendererCannotShowHardwareFrame(
            HardwareFrameRefusal::DriverCannotImport {
                details: "no sampler2D".to_owned(),
            },
        )
    }

    /// Отказ рендера в `auto` запускает программный фолбэк, как и составной multi-object.
    #[test]
    fn auto_accepts_software_fallback_for_a_renderer_refusal() {
        let mut controller = DmaBufRuntimeFallbackController::default();

        let selected = controller
            .begin(
                VideoBackendPreference::Auto,
                3,
                renderer_refusal(),
                Ok(software_plan()),
            )
            .expect("a renderer refusal must start the software fallback");

        assert_eq!(selected, software_plan());
    }

    /// `hardware` запрещает фолбэк и для отказа рендера; причина сохраняется целиком.
    #[test]
    fn hardware_preference_forbids_fallback_for_a_renderer_refusal() {
        let mut controller = DmaBufRuntimeFallbackController::default();

        let error = controller
            .begin(
                VideoBackendPreference::Hardware,
                3,
                renderer_refusal(),
                Ok(software_plan()),
            )
            .expect_err("hardware preference must reject the fallback");

        assert!(matches!(
            error,
            DmaBufRuntimeFallbackError::HardwareFallbackForbidden { trigger }
                if trigger == renderer_refusal()
        ));
    }

    /// Какие отказы склада кадров становятся причиной фолбэка, а какие нет.
    #[test]
    fn only_hardware_frame_refusals_become_fallback_triggers() {
        let renderer_reason = FrameImportUnsupportedReason::RendererCannotShowHardwareFrame(
            HardwareFrameRefusal::HostMemoryDescriptor,
        );
        assert_eq!(
            DmaBufFallbackTrigger::from_unsupported_reason(&renderer_reason),
            Some(DmaBufFallbackTrigger::RendererCannotShowHardwareFrame(
                HardwareFrameRefusal::HostMemoryDescriptor
            ))
        );
        let descriptor_reason = FrameImportUnsupportedReason::DmaBufDescriptorRejected(
            DmaBufDescriptorRejection::AbsentLayers,
        );
        assert_eq!(
            DmaBufFallbackTrigger::from_unsupported_reason(&descriptor_reason),
            Some(DmaBufFallbackTrigger::DescriptorRejected(
                DmaBufDescriptorRejection::AbsentLayers
            ))
        );
        for unrelated in [
            FrameImportUnsupportedReason::TransferPathNotHandledByActiveImport,
            FrameImportUnsupportedReason::LayoutNotHandledByActiveImport,
        ] {
            assert_eq!(
                DmaBufFallbackTrigger::from_unsupported_reason(&unrelated),
                None,
                "{unrelated:?}"
            );
        }
    }

    /// Лечится программным декодом: любой отказ рендера и составной multi-object; прочие
    /// отказы дескриптора остаются ошибкой (прежняя политика).
    #[test]
    fn software_decode_cures_renderer_refusals_and_composed_layouts_only() {
        assert!(renderer_refusal().is_cured_by_software_decode());
        assert!(unsupported_layout().is_cured_by_software_decode());
        assert!(
            !DmaBufFallbackTrigger::DescriptorRejected(DmaBufDescriptorRejection::AbsentLayers)
                .is_cured_by_software_decode()
        );
    }
}
