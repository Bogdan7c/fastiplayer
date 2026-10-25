//! Требование видеопотока, который играет прямо сейчас.
//!
//! Модуль — единственный владелец этого значения в `AppState`. Читатели (переподбор декода,
//! пересоздание рендера, live-пересборка по настройкам) берут его по `get`, а менять его
//! могут только intent-методы ниже. Так «какой файл играет» не расходится с «какой кодек
//! мы считаем активным».

use codec_core::VideoDecodeRequirement;

/// Требование активного видеопотока: `None`, пока видео не играет.
#[derive(Debug, Default)]
pub(super) struct ActiveVideoStreamRequirement {
    /// Требование потока, под который сейчас работает декодер.
    current: Option<VideoDecodeRequirement>,
}

impl ActiveVideoStreamRequirement {
    /// Текущее требование; `None` — видео не играет или требование ещё неизвестно.
    pub(super) fn get(&self) -> Option<&VideoDecodeRequirement> {
        self.current.as_ref()
    }

    /// Реактивный путь: player сам сообщил требование (событие переподбора декода).
    pub(super) fn adopt_reported(&mut self, requirement: VideoDecodeRequirement) {
        self.current = Some(requirement);
    }

    /// Commit кандидата: требование нового файла заменяет требование прошлого.
    ///
    /// `None` от кандидата (player его не приложил) прежнее требование не трогает: лучше
    /// помнить последнее известное, чем потерять фильтр выбора декодера.
    pub(super) fn adopt_committed(&mut self, committed: Option<VideoDecodeRequirement>) {
        if let Some(requirement) = committed {
            self.current = Some(requirement);
        }
    }

    /// Видео больше не играет (stop или медиа без видео): требование устарело.
    pub(super) fn clear(&mut self) {
        self.current = None;
    }
}

#[cfg(test)]
mod tests {
    use codec_core::VideoCodec;

    use super::*;

    fn requirement(codec: VideoCodec) -> VideoDecodeRequirement {
        VideoDecodeRequirement::new(codec)
    }

    #[test]
    fn first_committed_file_gets_its_own_requirement_instead_of_none() {
        let mut active = ActiveVideoStreamRequirement::default();
        assert!(active.get().is_none());

        active.adopt_committed(Some(requirement(VideoCodec::H264)));

        assert_eq!(active.get(), Some(&requirement(VideoCodec::H264)));
    }

    #[test]
    fn committed_next_file_replaces_previous_file_requirement() {
        let mut active = ActiveVideoStreamRequirement::default();
        active.adopt_committed(Some(requirement(VideoCodec::H264)));

        active.adopt_committed(Some(requirement(VideoCodec::Vp8)));

        assert_eq!(active.get(), Some(&requirement(VideoCodec::Vp8)));
    }

    #[test]
    fn candidate_without_requirement_keeps_last_known_one() {
        let mut active = ActiveVideoStreamRequirement::default();
        active.adopt_committed(Some(requirement(VideoCodec::H264)));

        active.adopt_committed(None);

        assert_eq!(active.get(), Some(&requirement(VideoCodec::H264)));
    }

    #[test]
    fn closing_media_clears_requirement() {
        let mut active = ActiveVideoStreamRequirement::default();
        active.adopt_committed(Some(requirement(VideoCodec::H264)));

        active.clear();

        assert!(active.get().is_none());
    }

    #[test]
    fn reported_requirement_overrides_committed_one() {
        let mut active = ActiveVideoStreamRequirement::default();
        active.adopt_committed(Some(requirement(VideoCodec::H264)));

        active.adopt_reported(requirement(VideoCodec::Av1));

        assert_eq!(active.get(), Some(&requirement(VideoCodec::Av1)));
    }
}
