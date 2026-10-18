//! Отложенный прогрев шрифтов потока-рисовальщика: «когда поток освободится, подготовь лица
//! для текста этой дорожки».
//!
//! Владелец решения «выполнять или нет» здесь, а не в цикле потока: цикл спрашивает только
//! [`PendingFontWarmUp::take`] в момент простоя. Правила (решение владельца: не тратить
//! память и диск впустую):
//! - в очереди живёт ОДИН запрос, новый заменяет прежний (побеждает последняя показанная дорожка);
//! - запрос выбрасывается, когда его дорожку снимают с показа (`Forget`) или зеркало
//!   заменяют другой дорожкой (`Replace` чужого источника): прогревать лица для невидимой
//!   дорожки бессмысленно;
//! - запрос выбрасывается при смене набора шрифтов: кэш лиц растеризатора всё равно сброшен,
//!   а результат поиска глифов устарел.

use subtitle_core::SubtitleLanguage;
use subtitle_raster::CueSourceId;

use super::commands::TrackCommand;

/// Запрос на прогрев: чей текст и на каком языке.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FontWarmUpRequest {
    /// Дорожка, ради которой греем.
    pub(super) source: CueSourceId,
    /// Образец текста дорожки (символы, которым нужны не встроенные лица).
    pub(super) sample: String,
    /// Язык дорожки (запасной язык системы подставляет поток).
    pub(super) language: Option<SubtitleLanguage>,
}

/// Ожидающий прогрев (не более одного).
#[derive(Debug, Default)]
pub(super) struct PendingFontWarmUp {
    request: Option<FontWarmUpRequest>,
}

impl PendingFontWarmUp {
    /// Запоминает запрос; прежний ожидающий запрос заменяется.
    pub(super) fn schedule(&mut self, request: FontWarmUpRequest) {
        self.request = Some(request);
    }

    /// Учитывает изменение зеркал: запрос снятой или вытесненной дорожки больше не нужен.
    pub(super) fn note_track_command(&mut self, command: &TrackCommand) {
        let dropped_source = match command {
            TrackCommand::Forget { source } => Some(*source),
            // Замена зеркала ДРУГОЙ дорожки: пользователь переключился, наша не показывается.
            TrackCommand::Replace { source, .. } => self
                .request
                .as_ref()
                .map(|request| request.source)
                .filter(|pending| pending != source),
            TrackCommand::Update { .. } => None,
        };
        let Some(dropped) = dropped_source else {
            return;
        };
        if self
            .request
            .as_ref()
            .is_some_and(|request| request.source == dropped)
        {
            self.request = None;
        }
    }

    /// Набор шрифтов изменился: ожидающий прогрев устарел.
    pub(super) fn discard(&mut self) {
        self.request = None;
    }

    /// Забирает запрос на выполнение (вызывать только когда поток простаивает).
    pub(super) fn take(&mut self) -> Option<FontWarmUpRequest> {
        self.request.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use subtitle_core::SubtitleTimeline;

    fn request(source: u64, sample: &str) -> FontWarmUpRequest {
        FontWarmUpRequest {
            source: CueSourceId::new(source),
            sample: sample.to_owned(),
            language: None,
        }
    }

    fn replace(source: u64) -> TrackCommand {
        TrackCommand::Replace {
            serial: 1,
            source: CueSourceId::new(source),
            language: None,
            timeline: SubtitleTimeline::new(),
        }
    }

    #[test]
    fn latest_request_wins_and_take_empties_the_queue() {
        let mut pending = PendingFontWarmUp::default();
        pending.schedule(request(1, "a"));
        pending.schedule(request(2, "b"));
        assert_eq!(pending.take(), Some(request(2, "b")));
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn replace_of_the_same_source_keeps_the_request_but_another_source_drops_it() {
        let mut pending = PendingFontWarmUp::default();
        pending.schedule(request(1, "a"));
        pending.note_track_command(&replace(1));
        assert_eq!(pending.take(), Some(request(1, "a")));
        pending.schedule(request(1, "a"));
        pending.note_track_command(&replace(2));
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn forget_drops_only_the_matching_request() {
        let mut pending = PendingFontWarmUp::default();
        pending.schedule(request(1, "a"));
        pending.note_track_command(&TrackCommand::Forget {
            source: CueSourceId::new(2),
        });
        assert_eq!(pending.take(), Some(request(1, "a")));
        pending.schedule(request(1, "a"));
        pending.note_track_command(&TrackCommand::Forget {
            source: CueSourceId::new(1),
        });
        assert_eq!(pending.take(), None);
    }

    #[test]
    fn discard_drops_the_request() {
        let mut pending = PendingFontWarmUp::default();
        pending.schedule(request(1, "a"));
        pending.discard();
        assert_eq!(pending.take(), None);
    }
}
