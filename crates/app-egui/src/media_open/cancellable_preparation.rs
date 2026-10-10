//! Фоновая подготовка источника с cooperative-отменой для владельцев вне coordinator-а.
//!
//! Coordinator сам ведёт `PreparationCancellation` своих request-ов. Пересборка media после
//! смены настроек (`state/settings_media_rebuild`) готовит источник в собственном потоке и
//! должна уметь прервать web-подготовку (сеть, yt-dlp) при shutdown, не раскрывая детали
//! внутреннего токена. Этот тип — узкая граница: «создать, отменить, подготовить».

use std::sync::Arc;

use player_core::MediaInstallCancellationCause;

use super::executor::PreparationCancellation;
use super::preparation::prepare_source;
use super::{MediaOpenSourceRequest, MediaPreparationFailureKind, PreparedMediaOpen};

/// Дешёвый клон общего флага отмены одной фоновой подготовки.
#[derive(Clone)]
pub(crate) struct SourcePreparationCancel {
    cancellation: Arc<PreparationCancellation>,
}

impl SourcePreparationCancel {
    /// Создаёт неотменённый токен.
    pub(crate) fn new() -> Self {
        Self {
            cancellation: Arc::new(PreparationCancellation::new()),
        }
    }

    /// Просит подготовку остановиться (cooperative): сеть и demux увидят тот же токен.
    pub(crate) fn cancel_for_lifecycle(&self) {
        self.cancellation
            .cancel(MediaInstallCancellationCause::LifecycleShutdown);
    }

    /// Видна ли отмена (нужно тестам: фоновая задача проверяет, что её действительно прервали).
    #[cfg(test)]
    pub(crate) fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    /// Подготавливает источник так же, как coordinator, но под этим токеном отмены.
    pub(crate) fn prepare(
        &self,
        source_request: MediaOpenSourceRequest,
    ) -> Result<PreparedMediaOpen, MediaPreparationFailureKind> {
        prepare_source(source_request, &self.cancellation)
    }
}
