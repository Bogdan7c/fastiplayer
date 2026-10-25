//! Commit подготовленного video-кандидата после player `Installed`.
//!
//! Владеет единственной точкой, где кандидат становится активным у приложения: импорт
//! ставится в рендер, класс backend-а и требование видеопотока обновляются вместе.

use render_shell_api::VideoFrameImport;

use super::{AppState, MediaInstallRequestId, ProductionCandidateOwner, StrongMediaOpenError};
use crate::video_pipeline_candidate::CommittedVideoPipelineParts;

impl AppState {
    /// После player Installed ставит импорт кадров кандидата в активный рендер.
    ///
    /// Прежний инвариант сохранён: кандидат заменяет уже установленный video pipeline. При
    /// mismatch request/generation активный импорт рендера и класс backend-а не меняются.
    pub(in crate::state) fn commit_installed_video_candidate(
        &mut self,
        video_frames: &mut dyn VideoFrameImport,
        candidate_owner: &ProductionCandidateOwner,
        player_request_id: MediaInstallRequestId,
    ) -> Result<(), StrongMediaOpenError> {
        if !candidate_owner.has_candidate() {
            // Кандидата нет — у установленного медиа нет видео, выбранного player-ом; старое
            // требование описывало бы уже закрытый файл.
            self.clear_active_video_stream_requirement();
            return Ok(());
        }
        if self.current_video_backend_kind.is_none()
            || video_frames.active_video_import_path().is_none()
        {
            return Err(StrongMediaOpenError::MissingActiveVideoPointers);
        }
        let committed = candidate_owner
            .commit_installed(player_request_id, self.renderer_generation)
            .map_err(StrongMediaOpenError::from)?;
        let CommittedVideoPipelineParts {
            backend_kind,
            stream_requirement,
            renderer_import,
        } = committed.into_parts();
        video_frames
            .install_video_import(renderer_import)
            .map_err(StrongMediaOpenError::RendererRejectedVideoImport)?;
        self.current_video_backend_kind = Some(backend_kind);
        // Требование меняется только после успешной установки импорта: отказ рендера
        // оставляет прежнее требование (старый pipeline остаётся активным).
        self.adopt_committed_video_stream_requirement(stream_requirement);
        Ok(())
    }
}
