//! Приём старта шкалы контейнера от плеера (см. [`super::container_start`]).

use std::time::Duration;

use player_core::MediaInstanceId;

use super::SubtitleOwner;
use super::container_start::ContainerStartReport;

impl SubtitleOwner {
    /// Сообщает, что демуксер локального файла опубликовал старт шкалы контейнера.
    ///
    /// Старт может прийти позже, чем прочитался файл рядом с видео (поиск идёт параллельно
    /// открытию), поэтому уже готовые файловые дорожки пересчитываются на месте. Показанная
    /// картинка забывается: её время поиска сдвинулось. Сообщение о чужом media (устаревший
    /// снимок) и о нелокальном источнике игнорируется. Возвращает `true`, если что-то
    /// изменилось и нужен ещё один кадр.
    pub(super) fn note_container_start(
        &mut self,
        media_instance_id: MediaInstanceId,
        start: Duration,
    ) -> bool {
        let Some(media) = self.media.as_mut() else {
            return false;
        };
        if media.media_instance_id != media_instance_id {
            return false;
        }
        match media.container_start.report(start) {
            ContainerStartReport::Changed(origin) => {
                media.tracks.apply_file_timeline_origin(origin);
                self.presenter.forget_shown_overlay();
                true
            }
            ContainerStartReport::Unchanged | ContainerStartReport::NotApplicable => false,
        }
    }
}
