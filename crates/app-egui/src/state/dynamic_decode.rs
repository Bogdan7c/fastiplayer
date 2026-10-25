//! Политика режима Dynamic: «аппаратный декодер не успевает — этот файл на программный».
//!
//! Детектор отставания живёт в player-core и только сообщает факт
//! (`PlayerEvent::VideoDecoderFallingBehind`). Решение «переключать ли» принимает этот модуль:
//!
//! * [`classify_decoder_pace_event`] — чистая функция-политика (всё, что известно о файле, —
//!   во входной структуре), поэтому проверяется без `AppState`;
//! * [`AppState::note_video_decoder_falling_behind`] — приём события: ставит повод в ту же
//!   очередь, что и фолбэк «рендер не умеет показать кадр» (`note_dma_buf_fallback_trigger`),
//!   сам переход делает прежний `apply_pending_dma_buf_runtime_fallback`;
//! * [`AppState::effective_video_backend_preference`] — «действующий режим текущего файла»:
//!   после успешного перехода Software, иначе committed preference;
//! * [`AppState::announce_dynamic_switch_to_software`] — тост и лог об успешном переходе.
//!
//! Инварианты:
//! * переход не более одного раза на файл (защёлка `DmaBufRuntimeFallbackController`);
//! * событие от программного декода никогда не возвращает на HW (нет пинг-понга): если и SW
//!   не успевает, остаёмся на SW;
//! * следующий файл снова начинает с HW (сброс на `MediaOpenRequested`). Это же верно для
//!   переоткрытия того же файла пересборкой по настройкам — известное допустимое поведение.

use fastiplayer_config::VideoBackendPreference;
use player_core::{
    DecoderPaceEvidence, MediaInstanceId, PlayerRenderError, PlayerRuntimeApplyError,
};
use tracing::{debug, info};

use super::AppState;
use super::VideoPipelineRebuildError;
use crate::dma_buf_runtime_fallback::{
    DmaBufFallbackTrigger, DmaBufRuntimeFallbackError, DmaBufRuntimeFallbackFailure,
};
use crate::video_pipeline_selector::VideoBackendKind;

/// Текст info-toast при успешном переходе на программный декод.
pub(crate) const DYNAMIC_SWITCH_TO_SOFTWARE_MESSAGE: &str =
    "Hardware decoding couldn't keep up, so this video switched to software decoding.";

/// Всё, что политике нужно знать о текущем файле и приложении.
pub(super) struct DecoderPaceEventContext {
    /// Медиа, которое сейчас играет (по последнему snapshot).
    pub(super) current_media: Option<MediaInstanceId>,
    /// Поколение render resources по последнему snapshot.
    pub(super) current_render_generation: u64,
    /// Класс установленного декодера; `None` — декодера нет.
    pub(super) current_backend_kind: Option<VideoBackendKind>,
    /// Committed preference пользователя (не «действующий» режим).
    pub(super) committed_preference: VideoBackendPreference,
    /// Уже была попытка перехода для этого файла (успешная или нет).
    pub(super) switch_already_attempted: bool,
}

/// Что делать с событием «декодер не успевает».
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DecoderPaceEventDisposition {
    /// Событие относится к другому медиа.
    IgnoreForeignMedia,
    /// Событие накоплено до последнего seek/смены декодера.
    IgnoreStaleGeneration,
    /// Сейчас не аппаратный декод: остаёмся на нём, обратного прыжка нет.
    IgnoreNotHardwareDecoding,
    /// Для этого файла переход уже пробовали: повтор не нужен.
    IgnoreSwitchAlreadyAttempted,
    /// Режим не Dynamic: медленность декодера переход не запускает.
    IgnorePreferenceDoesNotSwitch,
    /// Поставить переход на программный декод в очередь.
    QueueSoftwareSwitch,
}

/// Чистая политика приёма события отставания.
pub(super) fn classify_decoder_pace_event(
    evidence: &DecoderPaceEvidence,
    context: &DecoderPaceEventContext,
) -> DecoderPaceEventDisposition {
    if evidence.media_instance_id != context.current_media {
        return DecoderPaceEventDisposition::IgnoreForeignMedia;
    }
    // Событие из будущего относительно snapshot-а допустимо (snapshot мог отстать): отбрасываем
    // только заведомо старое.
    if evidence.render_generation < context.current_render_generation {
        return DecoderPaceEventDisposition::IgnoreStaleGeneration;
    }
    if context.current_backend_kind != Some(VideoBackendKind::HardwareZeroCopy) {
        return DecoderPaceEventDisposition::IgnoreNotHardwareDecoding;
    }
    if context.switch_already_attempted {
        return DecoderPaceEventDisposition::IgnoreSwitchAlreadyAttempted;
    }
    if context.committed_preference != VideoBackendPreference::Dynamic {
        return DecoderPaceEventDisposition::IgnorePreferenceDoesNotSwitch;
    }
    DecoderPaceEventDisposition::QueueSoftwareSwitch
}

/// Что отказ перехода Dynamic на SW значит для видео, которое сейчас играет.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RejectedSwitchImpact {
    /// Отказ случился до установки нового декодера: видео продолжает играть на HW, достаточно лога.
    VideoKeepsPlaying,
    /// SW-декодер уже установлен, но переход не дошёл до конца: видео сломано, нужна ошибка плееру.
    VideoBroken(PlayerRenderError),
}

/// Классифицирует отказ перехода, у которого нет собственной ошибки рендера (повод Dynamic).
///
/// Прежний повод «рендер не умеет показать кадр» несёт свою `PlayerRenderError` и сюда не
/// попадает: его маршрут не меняется.
pub(crate) fn classify_rejected_switch(
    failure: &DmaBufRuntimeFallbackFailure,
) -> RejectedSwitchImpact {
    let DmaBufRuntimeFallbackError::SoftwarePipelineFailed {
        fallback_failure, ..
    } = &failure.error
    else {
        return RejectedSwitchImpact::VideoKeepsPlaying;
    };
    let leaves_video_broken = matches!(
        fallback_failure,
        VideoPipelineRebuildError::ImportInstallAfterBackendSwap(_)
            | VideoPipelineRebuildError::Worker(
                PlayerRuntimeApplyError::ApplyAndRollbackFailed { .. }
            )
    );
    if leaves_video_broken {
        RejectedSwitchImpact::VideoBroken(PlayerRenderError::video_backend_switch_failed(format!(
            "Switching to software decoding failed: {fallback_failure}"
        )))
    } else {
        RejectedSwitchImpact::VideoKeepsPlaying
    }
}

impl AppState {
    /// Действующий режим выбора декодера для ТЕКУЩЕГО файла.
    ///
    /// После успешного перехода HW -> SW (любого повода) — `Software`, чтобы переподбор,
    /// проверка кандидата рендера и live-пересборка по настройкам не вернули HW посреди файла.
    /// Иначе — committed preference.
    pub(crate) fn effective_video_backend_preference(&self) -> VideoBackendPreference {
        self.dma_buf_runtime_fallback
            .effective_preference(self.video_backend_preference())
    }

    /// Принимает событие детектора: ставит переход на SW в очередь либо игнорирует событие.
    pub(crate) fn note_video_decoder_falling_behind(&mut self, evidence: DecoderPaceEvidence) {
        let context = DecoderPaceEventContext {
            current_media: self.last_player_snapshot.media_instance_id,
            current_render_generation: self.last_player_snapshot.render_generation,
            current_backend_kind: self.current_video_backend_kind,
            committed_preference: self.video_backend_preference(),
            switch_already_attempted: self.dma_buf_runtime_fallback.has_attempted_switch(),
        };
        match classify_decoder_pace_event(&evidence, &context) {
            DecoderPaceEventDisposition::QueueSoftwareSwitch => {
                self.note_dma_buf_fallback_trigger(
                    DmaBufFallbackTrigger::HardwareDecoderFallingBehind(evidence),
                    evidence.render_generation,
                    // Это не отказ кадра: плееру об ошибке рендера сообщать нечего.
                    None,
                );
            }
            ignored => debug!(
                disposition = ?ignored,
                "Decoder falling-behind event ignored: already on software decoding, wrong media, \
                 stale, or not in Dynamic mode; staying on the current decoder"
            ),
        }
    }

    /// Сообщает пользователю и логу, что файл переключён на программный декод.
    pub(crate) fn announce_dynamic_switch_to_software(&mut self, evidence: &DecoderPaceEvidence) {
        info!(
            expected_frames = evidence.expected_frames,
            late_frames = evidence.late_frames,
            starved_frames = evidence.starved_frames,
            missed_frame_ratio = evidence.missed_frame_ratio,
            playback_rate = %evidence.playback_rate,
            "Dynamic decode switched to software"
        );
        self.notify_info(DYNAMIC_SWITCH_TO_SOFTWARE_MESSAGE);
    }
}

#[cfg(test)]
mod tests;
