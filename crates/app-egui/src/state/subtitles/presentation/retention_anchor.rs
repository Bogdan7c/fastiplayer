//! «Где сейчас воспроизведение» для политики памяти картинных дорожек (PGS).
//!
//! Позиция берётся из наблюдения за плеером, а не из последнего показанного кадра субтитров:
//! при выключенных субтитрах (по умолчанию) кадры в презентатор не приходят, а фоновые
//! дорожки всё равно должны знать, какую часть хвоста держать.

use subtitle_core::{SubtitleSyncOffset, SubtitleTime};

use super::presenter::SubtitlePresenter;
use crate::state::subtitles::active_track::subtitle_time_from_pts;

impl SubtitlePresenter {
    /// Позиция воспроизведения во времени шкал дорожек (с учётом сдвига синхронизации); в seek
    /// и scrub — цель перехода, чтобы пакеты у цели не вытеснялись «по старой позиции».
    /// `None` — позиция ещё неизвестна: политика памяти не применяется.
    pub(in crate::state::subtitles) fn position_for_retention(
        &self,
        sync_offset: SubtitleSyncOffset,
    ) -> Option<SubtitleTime> {
        let position = self.observation.position?;
        Some(sync_offset.lookup_time_for(subtitle_time_from_pts(position)))
    }
}
