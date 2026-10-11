//! Политика видимости отказов demux при scrub-preview.
//!
//! Live-scrub preview — необязательная «картинка под курсором» во время перетаскивания
//! таймлайна; настоящий результат жеста даёт финальный seek по `EndScrub`. Если demuxer
//! источника не умеет синхронный seek для preview (так устроен progressive HTTP, где
//! финальный seek идёт через worker-receipted port), отсутствие preview не должно
//! превращаться в `RecoverableError`: пользователь увидел бы тост «Перемотка недоступна»
//! при удачной перемотке.

use frame_server_core::ScrubRequestKind;
use media_core::MediaDemuxError;

/// Нужно ли публиковать отказ demux seek-а как `RecoverableError`.
///
/// - `SeekLanding` — это сам seek пользователя: отказ всегда виден.
/// - `LiveScrub` — только preview: «seek недоступен» означает «preview не поддержан этим
///   источником» и скрывается (остаётся в логе); любой другой сбой demux остаётся видимым.
pub(super) fn demux_seek_failure_is_user_visible(
    request_kind: ScrubRequestKind,
    error: &anyhow::Error,
) -> bool {
    match request_kind {
        ScrubRequestKind::SeekLanding => true,
        ScrubRequestKind::LiveScrub => !error.chain().any(|cause| {
            cause
                .downcast_ref::<MediaDemuxError>()
                .is_some_and(MediaDemuxError::is_seek_unavailable)
        }),
    }
}

/// Нужно ли показывать общий отказ «landing не стартовал», когда конкретная причина не
/// была записана на месте сбоя.
///
/// Для `LiveScrub` — нет: preview не стартовал, но жест продолжается, и итог определит
/// финальный seek по `EndScrub` (он сам сообщит о своём отказе). Для `SeekLanding` —
/// да: это и есть seek пользователя, молчать о нём нельзя.
pub(super) const fn generic_landing_start_failure_is_user_visible(
    request_kind: ScrubRequestKind,
) -> bool {
    matches!(request_kind, ScrubRequestKind::SeekLanding)
}
