//! Controlled runtime fallback policy для DMA-BUF layout, известного только после first frame.
//!
//! Контроллер из этого модуля — «единственный на файл переход на программный декод»: его
//! защёлка пускает не больше одной попытки перехода HW -> SW на открытое медиа, какой бы
//! повод ни сработал первым (отказ рендера, отказ дескриптора или режим Dynamic: аппаратный
//! декодер не успевает).

use fastiplayer_config::VideoBackendPreference;
use player_core::DecoderPaceEvidence;
use render_shell_api::{FrameImportUnsupportedReason, HardwareFrameRefusal};
use video_core::DmaBufDescriptorRejection;

use crate::state::VideoPipelineRebuildError;
use crate::video_pipeline_selector::{VideoPipelinePlan, VideoPipelineSelectionError};

/// Причина, по которой аппаратный кадр нельзя показать и допустим программный фолбэк.
///
/// Два разных источника, не слитых в один: отказ дескриптора (его нашёл валидатор по данным
/// декодера) и отказ самого рендера (решение владельца 41: рендер не умеет показывать кадр).
/// Третий повод — режим Dynamic: аппаратный декодер устойчиво не успевает за видео.
///
/// `Eq` не выводится: доказательство отставания несёт дробные доли кадров (`f64`).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum DmaBufFallbackTrigger {
    /// Дескриптор DMA-BUF отклонён до импорта.
    DescriptorRejected(DmaBufDescriptorRejection),
    /// Рендер не может показать аппаратный кадр с исправным дескриптором.
    RendererCannotShowHardwareFrame(HardwareFrameRefusal),
    /// Аппаратный декодер устойчиво не успевает за видео (сигнал детектора player-core).
    /// Переход по этому поводу разрешён только режиму Dynamic.
    HardwareDecoderFallingBehind(DecoderPaceEvidence),
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
    /// Медленный аппаратный декодер лечится программным по замыслу режима Dynamic.
    pub(crate) const fn is_cured_by_software_decode(&self) -> bool {
        matches!(
            self,
            Self::RendererCannotShowHardwareFrame(_)
                | Self::HardwareDecoderFallingBehind(_)
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
            Self::HardwareDecoderFallingBehind(evidence) => write!(
                formatter,
                "hardware decoder is falling behind: {:.0}% of {:.0} expected frames missed",
                evidence.missed_frame_ratio * 100.0,
                evidence.expected_frames
            ),
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
    /// Typed player error с frame identity для финальной диагностики; `None` у поводов,
    /// не связанных с конкретным кадром (декодер не успевает): плееру об отказе сообщать нечего.
    pub(crate) player_error: Option<player_core::PlayerRenderError>,
}

/// Редкий error-path payload для shell reporting без раздувания success-path `Result`.
#[derive(Debug)]
pub(crate) struct DmaBufRuntimeFallbackFailure {
    /// Typed recovery policy/startup/restore error.
    pub(crate) error: DmaBufRuntimeFallbackError,
    /// Player-facing typed error с identity исходного frame lease-а; `None` — отказ перехода
    /// по поводу «декодер не успевает», который не должен превращаться в ошибку рендера.
    pub(crate) player_error: Option<player_core::PlayerRenderError>,
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
    /// Committed preference, при которой программный декод УСПЕШНО установлен для этого медиа.
    /// `None` — перехода не было (или он не удался).
    software_installed_under: Option<VideoBackendPreference>,
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
        let is_pace_trigger = matches!(
            trigger,
            DmaBufFallbackTrigger::HardwareDecoderFallingBehind(_)
        );
        match preference {
            VideoBackendPreference::Hardware => {
                return Err(DmaBufRuntimeFallbackError::HardwareFallbackForbidden { trigger });
            }
            VideoBackendPreference::Software => {
                return Err(DmaBufRuntimeFallbackError::FallbackNotApplicable { trigger });
            }
            // Auto по медленности декодера не переключает: это привилегия режима Dynamic.
            VideoBackendPreference::Auto if is_pace_trigger => {
                return Err(DmaBufRuntimeFallbackError::FallbackNotApplicable { trigger });
            }
            // Dynamic пускает любые поводы; Auto — прежние (не про медленность).
            VideoBackendPreference::Auto | VideoBackendPreference::Dynamic => {}
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

    /// Была ли для текущего медиа уже попытка перехода (успешная или нет).
    pub(crate) const fn has_attempted_switch(&self) -> bool {
        self.attempted_render_generation.is_some()
    }

    /// Фиксирует, что программный декод установлен при committed preference `preference`.
    pub(crate) fn note_software_switch_installed(&mut self, preference: VideoBackendPreference) {
        self.software_installed_under = Some(preference);
    }

    /// Действующий режим текущего файла.
    ///
    /// После успешного перехода — `Software`, чтобы читатели preference (переподбор, рендер,
    /// live-пересборка) не вернули аппаратный декод посреди файла. Если пользователь с тех пор
    /// сменил `video.preferred_backend`, его новый выбор главнее и override не действует.
    /// Известное ограничение: возврат к прежнему значению в том же файле снова включает override.
    pub(crate) fn effective_preference(
        &self,
        committed: VideoBackendPreference,
    ) -> VideoBackendPreference {
        if self.software_installed_under == Some(committed) {
            VideoBackendPreference::Software
        } else {
            committed
        }
    }

    /// Открытие нового media создаёт новый recovery scope: защёлка и override сбрасываются.
    ///
    /// Вызывается на каждое `MediaOpenRequested`, в том числе при переоткрытии того же файла
    /// пересборкой по настройкам — такой файл тоже начнёт с аппаратного декода (известное
    /// допустимое поведение).
    pub(crate) fn reset_for_new_media(&mut self) {
        self.attempted_render_generation = None;
        self.software_installed_under = None;
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
    use player_core::{DecoderPaceEvidence, PlayerVideoDecoderThreadConfig};

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
    fn dynamic_accepts_confirmed_software_fallback_like_auto() {
        let mut controller = DmaBufRuntimeFallbackController::default();

        let selected = controller
            .begin(
                VideoBackendPreference::Dynamic,
                7,
                unsupported_layout(),
                Ok(software_plan()),
            )
            .expect("Dynamic must allow the same confirmed software fallback as Auto");

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

    fn falling_behind() -> DmaBufFallbackTrigger {
        DmaBufFallbackTrigger::HardwareDecoderFallingBehind(DecoderPaceEvidence {
            evaluation_window: std::time::Duration::from_secs(4),
            expected_frames: 100.0,
            late_frames: 30,
            starved_frames: 10.0,
            missed_frame_ratio: 0.4,
            playback_rate: player_core::PlaybackRate::default(),
            media_instance_id: None,
            render_generation: 3,
        })
    }

    /// Dynamic: «не успевает» даёт подтверждённый программный план.
    #[test]
    fn dynamic_accepts_software_switch_for_falling_behind() {
        let mut controller = DmaBufRuntimeFallbackController::default();

        let selected = controller
            .begin(
                VideoBackendPreference::Dynamic,
                3,
                falling_behind(),
                Ok(software_plan()),
            )
            .expect("Dynamic must switch on a falling-behind decoder");

        assert_eq!(selected, software_plan());
    }

    /// Auto по медленности декодера не переключает, но прежние поводы обслуживает.
    #[test]
    fn auto_does_not_switch_for_falling_behind() {
        let mut controller = DmaBufRuntimeFallbackController::default();

        let error = controller
            .begin(
                VideoBackendPreference::Auto,
                3,
                falling_behind(),
                Ok(software_plan()),
            )
            .expect_err("Auto must not switch because of slowness");

        assert!(matches!(
            error,
            DmaBufRuntimeFallbackError::FallbackNotApplicable { .. }
        ));
        // Отказ по политике не тратит защёлку: прежний повод в Auto всё ещё работает.
        assert!(!controller.has_attempted_switch());
        controller
            .begin(
                VideoBackendPreference::Auto,
                3,
                renderer_refusal(),
                Ok(software_plan()),
            )
            .expect("the old trigger must still be served after a rejected slowness event");
    }

    #[test]
    fn hardware_forbids_and_software_does_not_apply_for_falling_behind() {
        let mut controller = DmaBufRuntimeFallbackController::default();
        let forbidden = controller
            .begin(
                VideoBackendPreference::Hardware,
                3,
                falling_behind(),
                Ok(software_plan()),
            )
            .expect_err("Hardware forbids software fallback");
        assert!(matches!(
            forbidden,
            DmaBufRuntimeFallbackError::HardwareFallbackForbidden { .. }
        ));

        let not_applicable = controller
            .begin(
                VideoBackendPreference::Software,
                3,
                falling_behind(),
                Ok(software_plan()),
            )
            .expect_err("Software has nothing to switch from");
        assert!(matches!(
            not_applicable,
            DmaBufRuntimeFallbackError::FallbackNotApplicable { .. }
        ));
    }

    /// Один переход на файл: второй повод (любой) блокируется, новое медиа снова разрешает.
    #[test]
    fn second_trigger_on_same_media_is_a_loop_and_new_media_reopens() {
        let mut controller = DmaBufRuntimeFallbackController::default();
        controller
            .begin(
                VideoBackendPreference::Dynamic,
                3,
                falling_behind(),
                Ok(software_plan()),
            )
            .expect("first switch is allowed");

        for second in [falling_behind(), renderer_refusal(), unsupported_layout()] {
            let error = controller
                .begin(
                    VideoBackendPreference::Dynamic,
                    3,
                    second,
                    Ok(software_plan()),
                )
                .expect_err("only one switch per file");
            assert!(matches!(
                error,
                DmaBufRuntimeFallbackError::FallbackLoopPrevented { .. }
            ));
        }

        controller.reset_for_new_media();
        controller
            .begin(
                VideoBackendPreference::Dynamic,
                4,
                falling_behind(),
                Ok(software_plan()),
            )
            .expect("the next file may switch again");
    }

    /// Dynamic с прежними поводами ведёт себя как Auto.
    #[test]
    fn dynamic_serves_old_triggers_like_auto() {
        for trigger in [renderer_refusal(), unsupported_layout()] {
            let mut controller = DmaBufRuntimeFallbackController::default();
            let selected = controller
                .begin(
                    VideoBackendPreference::Dynamic,
                    3,
                    trigger,
                    Ok(software_plan()),
                )
                .expect("Dynamic serves old triggers");
            assert_eq!(selected, software_plan());
        }
    }

    /// Действующий режим файла: committed -> Software после установки -> committed при смене
    /// настройки пользователем или новом медиа; неуспешная попытка режим не меняет.
    #[test]
    fn effective_preference_follows_the_installed_switch_only() {
        let mut controller = DmaBufRuntimeFallbackController::default();
        let dynamic = VideoBackendPreference::Dynamic;
        assert_eq!(controller.effective_preference(dynamic), dynamic);

        // Попытка без успешной установки (begin отдал план, но установка упала).
        controller
            .begin(dynamic, 3, falling_behind(), Ok(software_plan()))
            .expect("allowed");
        assert_eq!(
            controller.effective_preference(dynamic),
            dynamic,
            "a failed or not yet installed switch must not change the mode"
        );

        controller.note_software_switch_installed(dynamic);
        assert_eq!(
            controller.effective_preference(dynamic),
            VideoBackendPreference::Software
        );
        // Пользователь сам выбрал другое значение: его выбор главнее.
        assert_eq!(
            controller.effective_preference(VideoBackendPreference::Hardware),
            VideoBackendPreference::Hardware
        );

        controller.reset_for_new_media();
        assert_eq!(controller.effective_preference(dynamic), dynamic);
    }

    #[test]
    fn falling_behind_is_cured_by_software_and_has_english_display() {
        assert!(falling_behind().is_cured_by_software_decode());
        assert_eq!(
            falling_behind().to_string(),
            "hardware decoder is falling behind: 40% of 100 expected frames missed"
        );
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
