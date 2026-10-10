//! Различает «элемент очереди не открылся по своей вине» и «открыть мешает обстоятельство».
//!
//! Типизированная причина отказа (`MediaOpenUserFailureReason`) доходит сюда из strong-open;
//! раньше она сворачивалась в строку для бейджа, и очередь не могла отличить 404 от
//! пропавшей сети (B8). Теперь решение принимает владелец очереди по типу, а не по тексту.

use super::playlist_target_failure_summary;
use crate::media_open::MediaOpenUserFailureReason;
use crate::playlist_runtime::{PlaylistTargetFailure, QueueBlockReason};
use crate::state::StrongMediaOpenError;

/// Классифицирует отказ открытия элемента очереди для её владельца.
///
/// Остановить очередь могут только web-причины, которые `QueueBlockReason` признаёт
/// общими для соседей (нет сети, лимит запросов). Локальные файлы, отказы player-а и всё
/// неклассифицированное остаются отказом элемента с прежним бейджем и политикой skip.
pub(super) fn playlist_target_failure(error: &StrongMediaOpenError) -> PlaylistTargetFailure {
    if let Some(MediaOpenUserFailureReason::WebOpen(reason)) = error.user_failure_reason()
        && let Some(block) = QueueBlockReason::from_web_open_failure(reason)
    {
        return PlaylistTargetFailure::QueueBlocked(block);
    }
    PlaylistTargetFailure::ItemFault(playlist_target_failure_summary(error))
}
