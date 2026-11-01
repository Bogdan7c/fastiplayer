//! Подача полосы: текущий эпизод, курсор подачи и ключ эпизода.
//!
//! Полка решает, КАКОЙ сегмент следующий; подача решает, КУДА его положить:
//! в трубу текущего эпизода или в новый эпизод (свежий демуксер), если у сегмента
//! другой ключ (init, контейнер, смещение) или он не продолжает текущий эпизод.
//!
//! Курсор подачи ([`FeedCursor`]) — где кончается поданное (время показа).
//! Сегмент продолжает поданное, только если начинается у курсора; дальнейшее
//! правило — в [`crate::shelf::LaneShelf::next_to_feed`].
//!
//! **Предел ждущих эпизодов.** Каждый эпизод — поток демуксера и его очереди,
//! поэтому эпизодов, которые плеер ещё не забрал, на полосе не больше
//! `max_pending_episodes`. Подача с полки идёт с запасом до двух минут, и смена
//! качества впереди позиции начинает новый эпизод; если ящик полон, подача
//! останавливается ([`FeedAdmission::Deferred`]): сегменты ждут на полке, полоса
//! помечена «есть недоподанное» (`LaneMailbox::set_feed_backlog`), и подача
//! продолжается, когда плеер заберёт эпизод (см. `stream_buffer`).

use std::sync::Arc;

use crate::container::PushedContainer;
use crate::episode::{EpisodeFactory, EpisodeRecipe};
use crate::error::PushedStreamError;
use crate::framing::MediaTime;
use crate::intake::ChargedSegment;
use crate::lane::LaneId;
use crate::mailbox::{FeedBacklog, LaneMailbox, MailboxPost, SeekEpoch, StaleEpisodes};
use crate::pipe::{EpisodePipeWriter, PipeWrite};
use crate::shelf::IncomingMedia;
use crate::time::TimestampOffset;

/// Где кончается поданное полосы.
///
/// Две границы, потому что конец сегмента бывает известен неточно
/// ([`crate::framing::SegmentEnd`]): `certain_end` — докуда поданное известно
/// наверняка (сегмент, начавшийся раньше, — уже повтор), `reach` — докуда оно
/// может тянуться (сегмент, начавшийся до неё, — ещё продолжение). У точного
/// конца обе границы совпадают.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FeedCursor {
    /// Докуда поданное известно наверняка.
    certain_end: MediaTime,
    /// Докуда поданное может тянуться; не раньше `certain_end`.
    reach: MediaTime,
}

impl FeedCursor {
    /// Курсор из двух границ (досягаемость прижимается не раньше точного конца).
    pub(crate) fn new(certain_end: MediaTime, reach: MediaTime) -> Self {
        Self {
            certain_end,
            reach: reach.max(certain_end),
        }
    }

    /// Курсор точного конца.
    #[cfg(test)]
    pub(crate) fn exact(end: MediaTime) -> Self {
        Self::new(end, end)
    }

    /// Докуда поданное известно наверняка.
    pub(crate) const fn certain_end(self) -> MediaTime {
        self.certain_end
    }

    /// Докуда поданное может тянуться.
    pub(crate) const fn reach(self) -> MediaTime {
        self.reach
    }
}

/// Можно ли подать сегмент прямо сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FeedAdmission {
    /// Да: он продолжает текущий эпизод или в ящике есть место под новый.
    Now,
    /// Нет: нужен новый эпизод, а ящик полон — сегмент ждёт на полке.
    Deferred,
}

/// С чем играть сегмент: всё, что нужно для (пере)запуска эпизода с него.
///
/// Два сегмента — один эпизод, только если у них тот же принятый init (не просто
/// те же байты: init после `changeType` — новый), тот же контейнер и то же смещение.
#[derive(Debug, Clone)]
pub(crate) struct EpisodeKey {
    /// Контейнер полосы на момент приёма сегмента.
    container: PushedContainer,
    /// Init, к которому относится сегмент (у MPEG-TS его нет).
    initialization: Option<ChargedSegment>,
    /// Смещение времени полосы на момент приёма.
    offset: TimestampOffset,
}

impl EpisodeKey {
    /// Ключ из того, что действовало при приёме сегмента.
    pub(crate) fn new(
        container: PushedContainer,
        initialization: Option<ChargedSegment>,
        offset: TimestampOffset,
    ) -> Self {
        Self {
            container,
            initialization,
            offset,
        }
    }

    /// Можно ли играть сегменты с этими ключами одним демуксером.
    fn same_episode(&self, other: &Self) -> bool {
        let same_initialization = match (&self.initialization, &other.initialization) {
            (Some(mine), Some(theirs)) => mine.is_same_segment(theirs),
            (None, None) => true,
            _ => false,
        };
        self.container == other.container && self.offset == other.offset && same_initialization
    }
}

/// Можно ли положить сегмент в текущий эпизод.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EpisodeContinuity {
    /// Да, если ключ тот же.
    MayContinue,
    /// Нет: сегмент начинается раньше курсора, и демуксер получил бы время назад.
    MustRestart,
}

/// Эпизод, в который сейчас подаются сегменты.
#[derive(Debug)]
struct RunningEpisode {
    /// Труба эпизода.
    writer: EpisodePipeWriter,
    /// Ключ эпизода.
    key: EpisodeKey,
}

/// Подача одной полосы.
#[derive(Debug)]
pub(crate) struct LaneFeed {
    /// Номер полосы (для ошибок).
    lane: LaneId,
    /// Ящик эпизодов для плеера.
    mailbox: Arc<LaneMailbox>,
    /// Потолок ждущих эпизодов.
    max_pending_episodes: usize,
    /// Текущий эпизод; `None` — следующий сегмент начнёт новый.
    running: Option<RunningEpisode>,
    /// Где кончается поданное (`None` — ничего не подано с последней перемотки).
    cursor: Option<FeedCursor>,
    /// Эпоха подачи: эпизоды кладутся в ящик с ней (меняет только перемотка).
    epoch: SeekEpoch,
    /// Вход полосы окончен (`endOfStream`): поданное после перемотки дочитывается
    /// до конца сразу, а не ждёт байтов, которых не будет.
    input_ended: bool,
}

impl LaneFeed {
    /// Подача без эпизода и без курсора.
    pub(crate) fn new(
        lane: LaneId,
        mailbox: Arc<LaneMailbox>,
        max_pending_episodes: usize,
    ) -> Self {
        Self {
            lane,
            max_pending_episodes,
            epoch: mailbox.epoch(),
            mailbox,
            running: None,
            cursor: None,
            input_ended: false,
        }
    }

    /// Курсор подачи.
    pub(crate) const fn cursor(&self) -> Option<FeedCursor> {
        self.cursor
    }

    /// Сдвигает курсор к концу поданного сегмента (`fed_to` — курсор, который
    /// оставляет сам сегмент).
    ///
    /// Назад курсор здесь не ходит: сегмент из прошлого (точный конец раньше
    /// курсора) курсор не меняет. Вернуть его назад может только перемотка
    /// ([`Self::restart_for_seek`] снимает курсор, и следующий поданный сегмент
    /// задаёт его заново).
    pub(crate) fn advance_cursor(&mut self, fed_to: FeedCursor) {
        self.cursor = Some(match self.cursor {
            Some(cursor) if cursor.certain_end > fed_to.certain_end => cursor,
            Some(cursor) if cursor.certain_end == fed_to.certain_end => {
                FeedCursor::new(cursor.certain_end, cursor.reach.max(fed_to.reach))
            }
            _ => fed_to,
        });
    }

    /// Забывает курсор: полоса стала полосой без времени (MPEG-TS после
    /// `changeType`), прежняя шкала времени к её байтам больше не относится.
    pub(crate) fn forget_cursor(&mut self) {
        self.cursor = None;
    }

    /// Будет ли ещё вход у полосы.
    pub(crate) const fn incoming(&self) -> IncomingMedia {
        if self.input_ended {
            IncomingMedia::Ended
        } else {
            IncomingMedia::MayArrive
        }
    }

    /// Можно ли подать сегмент с ключом `key` прямо сейчас (правило — в
    /// описании модуля: новый эпизод требует места в ящике).
    pub(crate) fn admission(
        &self,
        key: &EpisodeKey,
        continuity: EpisodeContinuity,
    ) -> FeedAdmission {
        if self.continues_running(key, continuity)
            || self.mailbox.pending_episodes() < self.max_pending_episodes
        {
            FeedAdmission::Now
        } else {
            FeedAdmission::Deferred
        }
    }

    /// Отмечает, осталось ли у полосы недоподанное из-за полного ящика.
    pub(crate) fn note_backlog(&self, backlog: FeedBacklog) {
        self.mailbox.set_feed_backlog(backlog);
    }

    /// Осталось ли недоподанное из-за полного ящика.
    pub(crate) fn has_backlog(&self) -> bool {
        self.mailbox.has_feed_backlog()
    }

    /// Ляжет ли сегмент в текущий эпизод (без нового).
    fn continues_running(&self, key: &EpisodeKey, continuity: EpisodeContinuity) -> bool {
        continuity == EpisodeContinuity::MayContinue
            && self
                .running
                .as_ref()
                .is_some_and(|running| running.key.same_episode(key))
    }

    /// Кладёт сегмент в текущий эпизод или начинает с него новый.
    pub(crate) fn deliver(
        &mut self,
        media: ChargedSegment,
        key: &EpisodeKey,
        continuity: EpisodeContinuity,
        episodes: &EpisodeFactory,
    ) -> Result<(), PushedStreamError> {
        if !self.continues_running(key, continuity) {
            self.finish_running();
            return self.start_episode(media, key, episodes);
        }
        if let Some(running) = &self.running
            && running.writer.push(media) == PipeWrite::EpisodeAbandoned
        {
            // Эпизод выброшен плеером (поток закрыт): байты никому не нужны.
            self.running = None;
        }
        Ok(())
    }

    /// Текущий эпизод больше не получает байтов: его демуксер дочитает и закончит.
    pub(crate) fn finish_running(&mut self) {
        if let Some(running) = self.running.take() {
            running.writer.finish_input();
        }
    }

    /// Открыт ли вход полосы: страница ещё может прислать байты (не было
    /// `endOfStream` или вход открыт заново).
    pub(crate) const fn input_open(&self) -> bool {
        !self.input_ended
    }

    /// Конец входа полосы: эпизод дочитывается, плеер получит конец потока.
    pub(crate) fn end_input(&mut self) {
        self.input_ended = true;
        self.finish_running();
        self.mailbox.end_input();
    }

    /// Вход открыт снова (байты после `endOfStream` или перемотка туда, где
    /// байтов ещё нет): плеер ждёт новых эпизодов.
    pub(crate) fn reopen_input(&mut self) {
        self.input_ended = false;
        self.mailbox.reopen_input();
    }

    /// Перемотка: подача начинается заново с другого места.
    ///
    /// Текущий эпизод закончен, ждущие в ящике эпизоды старой позиции выгребены
    /// (уничтожить — вызывающему вне замков), начата новая эпоха, курсор снят:
    /// следующий поданный сегмент задаёт его заново — это единственный путь, по
    /// которому курсор уходит назад.
    pub(crate) fn restart_for_seek(&mut self) -> StaleEpisodes {
        self.finish_running();
        let (epoch, stale) = self.mailbox.restart_for_seek();
        self.epoch = epoch;
        self.cursor = None;
        stale
    }

    /// После подачи с полки: если вход уже окончен, текущий эпизод дочитывается
    /// до конца (иначе его демуксер ждал бы байтов, которых не будет).
    pub(crate) fn finish_running_if_input_ended(&mut self) {
        if self.input_ended {
            self.finish_running();
        }
    }

    /// Запускает новый эпизод с ключом сегмента и этим сегментом первым.
    fn start_episode(
        &mut self,
        first_media: ChargedSegment,
        key: &EpisodeKey,
        episodes: &EpisodeFactory,
    ) -> Result<(), PushedStreamError> {
        // Проверяем до запуска: у эпизода свой поток и свои очереди. Подача с
        // полки сюда не дойдёт (она спрашивает `admission` и ждёт); ошибка —
        // только для полосы без времени (MPEG-TS), которой ждать негде: страница,
        // которая так часто меняет `changeType`, — враждебная.
        if self.mailbox.pending_episodes() >= self.max_pending_episodes {
            return Err(PushedStreamError::TooManyPendingEpisodes {
                lane: self.lane,
                limit: self.max_pending_episodes,
            });
        }
        let recipe = EpisodeRecipe {
            container: key.container,
            initialization: key.initialization.as_ref(),
            first_media,
            offset: key.offset,
        };
        let (writer, handle) =
            episodes
                .start(recipe)
                .map_err(|error| PushedStreamError::EpisodeStart {
                    lane: self.lane,
                    reason: error.0,
                })?;
        match self.mailbox.post(self.epoch, handle) {
            MailboxPost::Posted => {
                self.running = Some(RunningEpisode {
                    writer,
                    key: key.clone(),
                });
            }
            // Плеер закрыл поток или эпизод из старой эпохи: байты никому не нужны.
            MailboxPost::ConsumerClosed | MailboxPost::StaleEpoch => {}
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{FeedCursor, LaneFeed};
    use crate::framing::MediaTime;
    use crate::lane::LaneId;
    use crate::mailbox::LaneMailbox;

    fn at_ms(milliseconds: i64) -> Option<FeedCursor> {
        Some(FeedCursor::exact(MediaTime::from_micros(
            milliseconds * 1_000,
        )))
    }

    #[test]
    fn cursor_moves_back_only_through_a_seek_restart() {
        let mailbox = Arc::new(LaneMailbox::default());
        let mut feed = LaneFeed::new(LaneId::new(0), Arc::clone(&mailbox), 4);
        feed.advance_cursor(FeedCursor::exact(MediaTime::from_micros(800_000)));
        // Подача сегмента из прошлого курсор назад не двигает.
        feed.advance_cursor(FeedCursor::exact(MediaTime::from_micros(400_000)));
        assert_eq!(feed.cursor(), at_ms(800));

        let epoch_before = mailbox.epoch();
        let stale = feed.restart_for_seek();
        assert_eq!(stale.len(), 0);
        assert_eq!(feed.cursor(), None, "a seek drops the cursor");
        assert_ne!(mailbox.epoch(), epoch_before, "a seek starts a new epoch");
        feed.advance_cursor(FeedCursor::exact(MediaTime::from_micros(400_000)));
        assert_eq!(feed.cursor(), at_ms(400));
    }
}
