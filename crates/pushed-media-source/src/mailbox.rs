//! Ящик эпизодов полосы: очередь запущенных эпизодов между приёмом и плеером.
//!
//! Кладёт подача полосы (`feed`, под замком буфера потока), забирает демуксер
//! полосы на стороне плеера (`lane_demuxer`). Флаги «вход окончен», «плеер
//! закрыл поток» и «есть недоподанное» живут здесь же.
//!
//! **Недоподанное** ([`FeedBacklog`]). Подача с полки останавливается, когда ящик
//! полон (предел ждущих эпизодов); сегменты ждут на полке. Пока так, опустевший
//! ящик НЕ означает конец потока, даже после `endOfStream`: плеер ждёт, а сторона
//! плеера, забрав эпизод, просит буфер продолжить подачу (без ожидания замка —
//! `StreamBuffer::try_catch_up_feeding`).
//!
//! **Эпоха перемотки.** Перемотка начинает подачу заново с другого места: всё,
//! что лежало в ящике, относится к старой позиции. [`LaneMailbox::restart_for_seek`]
//! выгребает такие эпизоды и начинает новую эпоху; эпизод помнит эпоху, в которой
//! его положили, и [`LaneMailbox::take_next`] никогда не отдаёт плееру эпизод
//! старой эпохи. Сейчас подача и перемотка обе идут под замком буфера, так что
//! старый эпизод и не может появиться после выгребания; эпоха — страховка
//! инварианта «старое время не доходит до плеера» на случай, если запуск
//! эпизодов когда-нибудь уедет из-под этого замка.

use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

use crate::episode::EpisodeHandle;

/// Номер эпохи подачи полосы (растёт с каждой перемоткой).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct SeekEpoch(u64);

impl SeekEpoch {
    /// Следующая эпоха.
    const fn next(self) -> Self {
        Self(self.0.wrapping_add(1))
    }
}

/// Эпизоды старой позиции, выгребенные перемоткой.
///
/// Их нужно уничтожить ВНЕ замков: `Drop` эпизода будит поток его демуксера.
#[derive(Debug, Default)]
#[must_use = "stale episodes are dropped by the caller outside of the buffer lock"]
pub(crate) struct StaleEpisodes(Vec<EpisodeHandle>);

impl StaleEpisodes {
    /// Добавляет эпизоды другой полосы.
    pub(crate) fn absorb(&mut self, other: Self) {
        self.0.extend(other.0);
    }

    /// Сколько эпизодов выброшено (для тестов и диагностики).
    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }
}

/// Осталось ли у полосы недоподанное из-за полного ящика.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum FeedBacklog {
    /// Подано всё, что можно подать.
    #[default]
    Clear,
    /// На полке ждут сегменты, которые пойдут, когда плеер заберёт эпизод.
    Pending,
}

/// Чем закончилась попытка положить эпизод.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailboxPost {
    /// Эпизод ждёт плеер.
    Posted,
    /// Плеер уже закрыл поток: эпизод выброшен.
    ConsumerClosed,
    /// Эпизод из эпохи до перемотки: выброшен, плеер его не увидит.
    StaleEpoch,
}

/// Очередь эпизодов между приёмом и плеером.
#[derive(Debug, Default)]
pub(crate) struct LaneMailbox {
    /// Состояние под замком.
    state: Mutex<MailboxState>,
}

/// Состояние почтового ящика.
#[derive(Debug, Default)]
struct MailboxState {
    /// Запущенные, но ещё не забранные плеером эпизоды и эпоха каждого.
    episodes: VecDeque<(SeekEpoch, EpisodeHandle)>,
    /// Текущая эпоха подачи.
    epoch: SeekEpoch,
    /// Новых эпизодов не будет.
    input_ended: bool,
    /// Плеер закрыл поток.
    consumer_closed: bool,
    /// Недоподанное из-за полного ящика.
    feed_backlog: FeedBacklog,
}

/// Что лежит в ящике для плеера.
#[derive(Debug)]
pub(crate) enum MailboxTake {
    /// Следующий эпизод (в куче: ручка эпизода крупная, а ящик отдаёт её редко).
    Episode(Box<EpisodeHandle>),
    /// Пока пусто, вход продолжается.
    Waiting,
    /// Пусто, и новых эпизодов не будет.
    InputEnded,
}

impl LaneMailbox {
    /// Текущая эпоха (подача запоминает её и кладёт эпизоды с ней).
    pub(crate) fn epoch(&self) -> SeekEpoch {
        self.lock().epoch
    }

    /// Кладёт эпизод, запущенный в эпоху `epoch`.
    pub(crate) fn post(&self, epoch: SeekEpoch, episode: EpisodeHandle) -> MailboxPost {
        let mut state = self.lock();
        if state.consumer_closed {
            return MailboxPost::ConsumerClosed;
        }
        if epoch != state.epoch {
            // Старый эпизод уничтожается уже после отпускания замка ящика.
            drop(state);
            drop(episode);
            return MailboxPost::StaleEpoch;
        }
        state.episodes.push_back((epoch, episode));
        MailboxPost::Posted
    }

    /// Перемотка: новая эпоха; ждущие эпизоды старой позиции — вызывающему на
    /// уничтожение вне замков.
    pub(crate) fn restart_for_seek(&self) -> (SeekEpoch, StaleEpisodes) {
        let mut state = self.lock();
        state.epoch = state.epoch.next();
        // Недоподанное старой позиции больше не нужно: подача начнётся заново.
        state.feed_backlog = FeedBacklog::Clear;
        let stale = std::mem::take(&mut state.episodes)
            .into_iter()
            .map(|(_, episode)| episode)
            .collect();
        (state.epoch, StaleEpisodes(stale))
    }

    /// Сколько эпизодов ждут плеер.
    pub(crate) fn pending_episodes(&self) -> usize {
        self.lock().episodes.len()
    }

    /// Новых эпизодов пока не будет: опустевший ящик означает конец потока.
    pub(crate) fn end_input(&self) {
        self.lock().input_ended = true;
    }

    /// Вход снова открыт (MSE: `appendBuffer` после `endOfStream`, или перемотка
    /// туда, где байтов ещё нет): опустевший ящик снова означает «подожди».
    pub(crate) fn reopen_input(&self) {
        self.lock().input_ended = false;
    }

    /// Отмечает, осталось ли недоподанное из-за полного ящика.
    pub(crate) fn set_feed_backlog(&self, backlog: FeedBacklog) {
        self.lock().feed_backlog = backlog;
    }

    /// Есть ли недоподанное: стороне плеера стоит попросить подачу продолжить.
    pub(crate) fn has_feed_backlog(&self) -> bool {
        self.lock().feed_backlog == FeedBacklog::Pending
    }

    /// Закрыл ли плеер поток.
    pub(crate) fn consumer_closed(&self) -> bool {
        self.lock().consumer_closed
    }

    /// Следующий эпизод текущей эпохи для плеера.
    pub(crate) fn take_next(&self) -> MailboxTake {
        let mut stale = Vec::new();
        let take = {
            let mut state = self.lock();
            let current_epoch = state.epoch;
            loop {
                match state.episodes.pop_front() {
                    Some((epoch, episode)) if epoch == current_epoch => {
                        break MailboxTake::Episode(Box::new(episode));
                    }
                    // Эпизод старой эпохи плееру не отдаётся никогда.
                    Some((_, episode)) => stale.push(episode),
                    // Недоподанное ещё придёт: это не конец, даже после `endOfStream`.
                    None if state.input_ended && state.feed_backlog == FeedBacklog::Clear => {
                        break MailboxTake::InputEnded;
                    }
                    None => break MailboxTake::Waiting,
                }
            }
        };
        // Старые эпизоды уничтожаются вне замка.
        drop(stale);
        take
    }

    /// Плеер закрыл поток: ждущие эпизоды выбрасываются, новые не принимаются.
    pub(crate) fn close_consumer(&self) {
        let abandoned = {
            let mut state = self.lock();
            state.consumer_closed = true;
            std::mem::take(&mut state.episodes)
        };
        // Эпизоды уничтожаются вне замка: их Drop будит потоки демуксеров.
        drop(abandoned);
    }

    /// Захватывает состояние; отравленный mutex не мешает закрытию.
    fn lock(&self) -> MutexGuard<'_, MailboxState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::{LaneMailbox, MailboxPost, MailboxTake};
    use crate::container::PushedContainer;
    use crate::episode::{EpisodeFactory, EpisodeHandle, EpisodeRecipe};
    use crate::intake::{ChargedSegment, IntakeLedger};
    use crate::policy::PushedSourcePolicy;
    use crate::stream::pushed_demux_registry;
    use crate::test_fixtures::{VIDEO_H264_64, split_mp4};
    use crate::time::TimestampOffset;

    /// Настоящий эпизод (init + первый фрагмент фикстуры).
    fn episode() -> EpisodeHandle {
        let policy = PushedSourcePolicy::recommended().expect("policy");
        let factory =
            EpisodeFactory::new(Arc::new(pushed_demux_registry().expect("registry")), policy);
        let ledger = Arc::new(IntakeLedger::new(1 << 20));
        let media = split_mp4(VIDEO_H264_64);
        let charge = |bytes: &[u8]| {
            ChargedSegment::charge(&ledger, bytes::Bytes::copy_from_slice(bytes)).expect("charge")
        };
        let initialization = charge(&media.init);
        let (_writer, handle) = factory
            .start(EpisodeRecipe {
                container: PushedContainer::FragmentedMp4,
                initialization: Some(&initialization),
                first_media: charge(&media.fragments[0]),
                offset: TimestampOffset::ZERO,
            })
            .expect("episode");
        handle
    }

    #[test]
    fn seek_epoch_discards_waiting_and_late_episodes_of_the_old_position() {
        let mailbox = LaneMailbox::default();
        let old_epoch = mailbox.epoch();
        assert_eq!(mailbox.post(old_epoch, episode()), MailboxPost::Posted);

        let (new_epoch, stale) = mailbox.restart_for_seek();
        assert_ne!(new_epoch, old_epoch);
        assert_eq!(
            stale.len(),
            1,
            "the waiting episode belongs to the old position"
        );
        assert_eq!(mailbox.pending_episodes(), 0);
        // Эпизод, запущенный до перемотки и положенный после неё, плееру не попадёт.
        assert_eq!(mailbox.post(old_epoch, episode()), MailboxPost::StaleEpoch);
        assert!(matches!(mailbox.take_next(), MailboxTake::Waiting));

        assert_eq!(mailbox.post(new_epoch, episode()), MailboxPost::Posted);
        assert!(matches!(mailbox.take_next(), MailboxTake::Episode(_)));
    }

    #[test]
    fn closed_consumer_wins_over_epochs() {
        let mailbox = LaneMailbox::default();
        mailbox.close_consumer();
        assert_eq!(
            mailbox.post(mailbox.epoch(), episode()),
            MailboxPost::ConsumerClosed
        );
    }
}
