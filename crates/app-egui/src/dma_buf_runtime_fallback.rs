//! Controlled runtime fallback policy для DMA-BUF layout, известного только после first frame.

use fastiplayer_config::VideoBackendPreference;
use video_core::DmaBufDescriptorRejection;

use crate::state::VideoPipelineRebuildError;
use crate::video_pipeline_selector::{VideoPipelinePlan, VideoPipelineSelectionError};

/// Pending runtime rejection, перенесённая из materialization stage к composition boundary.
#[derive(Debug)]
pub(crate) struct PendingDmaBufLayoutRejection {
    /// Исходная typed descriptor/layout ошибка.
    pub(crate) layout_rejection: DmaBufDescriptorRejection,
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
        /// Исходная typed descriptor/layout ошибка.
        layout_rejection: DmaBufDescriptorRejection,
    },
    /// Текущий backend не является `auto` hardware path-ом, поэтому fallback неприменим.
    FallbackNotApplicable {
        /// Исходная typed descriptor/layout ошибка.
        layout_rejection: DmaBufDescriptorRejection,
    },
    /// Для этого media generation controlled fallback уже пытались выполнить.
    FallbackLoopPrevented {
        /// Исходная typed descriptor/layout ошибка.
        layout_rejection: DmaBufDescriptorRejection,
        /// Render generation, к которому привязана единственная попытка.
        render_generation: u64,
    },
    /// Capability snapshot не содержит заранее подтверждённого playable software plan-а.
    SoftwarePipelineUnavailable {
        /// Исходная typed descriptor/layout ошибка.
        layout_rejection: DmaBufDescriptorRejection,
        /// Typed причина отказа selector-а.
        selection_error: VideoPipelineSelectionError,
    },
    /// Software backend startup либо worker restore/commit завершились ошибкой.
    SoftwarePipelineFailed {
        /// Исходная typed descriptor/layout ошибка.
        layout_rejection: DmaBufDescriptorRejection,
        /// Typed failure controlled pipeline rebuild-а.
        fallback_failure: VideoPipelineRebuildError,
    },
}

impl std::fmt::Display for DmaBufRuntimeFallbackError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HardwareFallbackForbidden { layout_rejection } => write!(
                formatter,
                "unsupported DMA-BUF layout: {layout_rejection}; software fallback is forbidden by video.preferred_backend=hardware"
            ),
            Self::FallbackNotApplicable { layout_rejection } => write!(
                formatter,
                "unsupported DMA-BUF layout: {layout_rejection}; runtime software fallback is not applicable to the active preference"
            ),
            Self::FallbackLoopPrevented {
                layout_rejection,
                render_generation,
            } => write!(
                formatter,
                "unsupported DMA-BUF layout: {layout_rejection}; fallback loop prevented for render generation {render_generation}"
            ),
            Self::SoftwarePipelineUnavailable {
                layout_rejection,
                selection_error,
            } => write!(
                formatter,
                "unsupported DMA-BUF layout: {layout_rejection}; confirmed software fallback is unavailable: {selection_error}"
            ),
            Self::SoftwarePipelineFailed {
                layout_rejection,
                fallback_failure,
            } => write!(
                formatter,
                "unsupported DMA-BUF layout: {layout_rejection}; controlled software fallback failed: {fallback_failure}"
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
        layout_rejection: DmaBufDescriptorRejection,
        software_plan: Result<VideoPipelinePlan, VideoPipelineSelectionError>,
    ) -> Result<VideoPipelinePlan, DmaBufRuntimeFallbackError> {
        match preference {
            VideoBackendPreference::Hardware => {
                return Err(DmaBufRuntimeFallbackError::HardwareFallbackForbidden {
                    layout_rejection,
                });
            }
            VideoBackendPreference::Software => {
                return Err(DmaBufRuntimeFallbackError::FallbackNotApplicable { layout_rejection });
            }
            VideoBackendPreference::Auto => {}
        }

        if self.attempted_render_generation.is_some() {
            return Err(DmaBufRuntimeFallbackError::FallbackLoopPrevented {
                layout_rejection,
                render_generation,
            });
        }
        // Guard фиксируется до selection/startup: unavailable и failed попытки тоже не
        // должны повторяться на каждом redraw и образовывать recovery loop.
        self.attempted_render_generation = Some(render_generation);

        software_plan.map_err(|selection_error| {
            DmaBufRuntimeFallbackError::SoftwarePipelineUnavailable {
                layout_rejection,
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
        layout_rejection: DmaBufDescriptorRejection,
        fallback_failure: VideoPipelineRebuildError,
    ) -> DmaBufRuntimeFallbackError {
        DmaBufRuntimeFallbackError::SoftwarePipelineFailed {
            layout_rejection,
            fallback_failure,
        }
    }
}

#[cfg(test)]
mod tests {
    use player_core::PlayerVideoDecoderThreadConfig;

    use super::*;

    fn unsupported_layout() -> DmaBufDescriptorRejection {
        DmaBufDescriptorRejection::UnsupportedComposedMultiObject {
            first_object_index: 0,
            conflicting_object_index: 1,
        }
    }

    fn software_plan() -> VideoPipelinePlan {
        VideoPipelinePlan::FfmpegHostUploadWgpu {
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
                layout_rejection: DmaBufDescriptorRejection::UnsupportedComposedMultiObject { .. },
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
                layout_rejection: DmaBufDescriptorRejection::UnsupportedComposedMultiObject { .. },
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
}
