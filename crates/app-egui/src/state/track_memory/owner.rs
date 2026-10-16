//! Владелец памяти выбора: данные в памяти процесса + отложенная запись на диск.
//!
//! # Инварианты
//! - Память в оперативке обновляется МГНОВЕННО при каждом действии пользователя: повторная
//!   установка того же видео в этой же сессии (пересборка после настроек, Previous/Next)
//!   восстанавливает выбор без обращения к диску.
//! - Запись на диск — «только последнее», в фоне, с паузой покоя
//!   ([`TRACK_MEMORY_SAVE_DEBOUNCE`]); сразу — при смене media, при suspend и при выходе.
//! - Запись не блокирует интерфейс и не влияет на воспроизведение: любой сбой — предупреждение
//!   в логе без путей, память продолжает работать в оперативке.
//! - Изменение относится к конкретному media (`MediaInstanceId`); устаревшее (media уже
//!   сменилось) отбрасывается типизированным исходом, а не пишется в чужую запись.
//! - Для источника без ключа (экзотическая платформа, пустая ссылка) память не работает.

use std::num::NonZeroU64;
use std::time::{Duration, Instant};

use player_core::MediaInstanceId;
use playlist_state::{
    RememberOutcome, RememberedSubtitleChoice, RememberedTrackHint, SubtitleSyncOffset,
    TouchOutcome, TrackMemory, TrackMemoryCapacity, TrackMemorySaveRevision,
    TrackMemoryWriteReport, TrackMemoryWriteSnapshot, TrackMemoryWriter,
    TrackMemoryWriterShutdownOutcome, VideoKey,
};

/// Пауза покоя перед записью на диск (как у сохранения громкости и «субтитры включены»).
pub(crate) const TRACK_MEMORY_SAVE_DEBOUNCE: Duration = Duration::from_secs(3);

/// Что память помнит про текущее видео.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RememberedForVideo {
    /// Запомненная аудиодорожка.
    pub(crate) audio: Option<RememberedTrackHint>,
    /// Запомненный выбор субтитров.
    pub(crate) subtitles: Option<RememberedSubtitleChoice>,
    /// Запомненный сдвиг субтитров.
    pub(crate) sync_offset: SubtitleSyncOffset,
}

impl RememberedForVideo {
    /// Для видео ничего не запомнено.
    pub(crate) const fn nothing() -> Self {
        Self {
            audio: None,
            subtitles: None,
            sync_offset: SubtitleSyncOffset::ZERO,
        }
    }
}

/// Куда делось изменение, которое пользователь сделал в media `media_instance_id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RecordOutcome {
    /// Память уже содержит это значение: ничего не изменилось, писать нечего.
    Unchanged,
    /// Значение сохранено в памяти (диск обновится после паузы покоя).
    Stored,
    /// У текущего media нет ключа или оно уже не то, что в событии: изменение отброшено.
    NotApplicable,
}

/// Как память переживает сессию.
pub(super) enum Persistence {
    /// Только оперативка (причина - в исходе запуска `TrackMemoryStartOutcome`).
    InMemoryOnly,
    /// Фоновая запись включена.
    Writing(WritingState),
}

/// Состояние фоновой записи.
pub(super) struct WritingState {
    /// Поток записи «только последнее».
    pub(super) writer: TrackMemoryWriter,
    /// Номер ревизии, который получит следующий снимок.
    pub(super) next_revision: u64,
    /// Когда запись станет due: `None` — несохранённых изменений нет.
    pub(super) save_due_at: Option<Instant>,
    /// Ревизия последнего отчёта, о котором уже написали в лог.
    pub(super) last_logged_revision: Option<TrackMemorySaveRevision>,
}

/// Итог завершения записи при выходе.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackMemoryShutdownOutcome {
    /// Фоновая запись выключена, сбрасывать нечего.
    NothingToFlush,
    /// Всё записано (отчёт последней попытки, если была).
    Completed(Option<TrackMemoryWriteReport>),
    /// Поток не успел за отведённое время.
    TimedOut,
    /// Поток записи недоступен или уже завершён.
    WriterUnavailable,
}

/// Видео, для которого сейчас действует память.
#[derive(Clone, Copy)]
struct CurrentVideo {
    key: VideoKey,
    media_instance_id: MediaInstanceId,
}

/// Владелец памяти выбора процесса.
pub(crate) struct TrackMemoryOwner {
    memory: TrackMemory,
    current: Option<CurrentVideo>,
    persistence: Persistence,
}

impl TrackMemoryOwner {
    pub(super) fn new(memory: TrackMemory, persistence: Persistence) -> Self {
        Self {
            memory,
            current: None,
            persistence,
        }
    }

    /// Установлено media: прежние несохранённые изменения уходят на запись сразу, ключ
    /// переключается, запись видео освежается (защита от вытеснения).
    ///
    /// `key = None` — для источника память не работает.
    pub(crate) fn begin_video(
        &mut self,
        media_instance_id: MediaInstanceId,
        key: Option<VideoKey>,
        now: Instant,
    ) {
        self.flush_unsaved_changes();
        self.current = key.map(|key| CurrentVideo {
            key,
            media_instance_id,
        });
        let Some(current) = self.current else {
            return;
        };
        if self.memory.touch(&current.key) == TouchOutcome::Touched {
            self.mark_unsaved(now);
        }
    }

    /// Media исчезло из плеера: память больше ни к чему не привязана.
    pub(crate) fn end_video(&mut self) {
        self.flush_unsaved_changes();
        self.current = None;
    }

    /// Что помнит память про видео установленного media (пусто, если media не то).
    pub(crate) fn remembered_for(&self, media_instance_id: MediaInstanceId) -> RememberedForVideo {
        match self.current_for(media_instance_id) {
            Some(current) => self.remembered_for_key(&current.key),
            None => RememberedForVideo::nothing(),
        }
    }

    /// Что помнит память про видео с этим ключом. Нужен звуку: его восстановление идёт
    /// до публикации установленного media, когда `begin_video` ещё не вызывался.
    pub(crate) fn remembered_for_key(&self, key: &VideoKey) -> RememberedForVideo {
        let Some(entry) = self.memory.entry_for(key) else {
            return RememberedForVideo::nothing();
        };
        RememberedForVideo {
            audio: entry.audio().cloned(),
            subtitles: entry.subtitles().cloned(),
            sync_offset: entry.subtitle_sync_offset(),
        }
    }

    /// Запоминает аудиодорожку, выбранную пользователем.
    pub(crate) fn remember_audio(
        &mut self,
        media_instance_id: MediaInstanceId,
        audio: RememberedTrackHint,
        now: Instant,
    ) -> RecordOutcome {
        let Some(current) = self.current_for(media_instance_id) else {
            return RecordOutcome::NotApplicable;
        };
        let outcome = self.memory.remember_audio(current.key, audio);
        self.after_remember(outcome, now)
    }

    /// Запоминает выбор субтитров, сделанный пользователем (включая «выключено»).
    pub(crate) fn remember_subtitles(
        &mut self,
        media_instance_id: MediaInstanceId,
        choice: RememberedSubtitleChoice,
        now: Instant,
    ) -> RecordOutcome {
        let Some(current) = self.current_for(media_instance_id) else {
            return RecordOutcome::NotApplicable;
        };
        let outcome = self.memory.remember_subtitles(current.key, choice);
        self.after_remember(outcome, now)
    }

    /// Запоминает сдвиг субтитров, заданный пользователем.
    pub(crate) fn remember_sync_offset(
        &mut self,
        media_instance_id: MediaInstanceId,
        offset: SubtitleSyncOffset,
        now: Instant,
    ) -> RecordOutcome {
        let Some(current) = self.current_for(media_instance_id) else {
            return RecordOutcome::NotApplicable;
        };
        let outcome = self.memory.remember_sync_offset(current.key, offset);
        self.after_remember(outcome, now)
    }

    /// Новая ёмкость из настроек. Возвращает число вытесненных записей.
    pub(crate) fn apply_capacity(&mut self, capacity: TrackMemoryCapacity, now: Instant) -> usize {
        if self.memory.capacity() == capacity {
            return 0;
        }
        let evicted = self.memory.set_capacity(capacity);
        if evicted > 0 {
            self.mark_unsaved(now);
        }
        evicted
    }

    /// Периодический шаг: отправляет запись, когда пауза покоя прошла, и сообщает в лог о
    /// сбоях записи. Неблокирующий.
    pub(crate) fn poll(&mut self, now: Instant) {
        let due = matches!(
            &self.persistence,
            Persistence::Writing(state) if state.save_due_at.is_some_and(|due_at| now >= due_at)
        );
        if due {
            self.submit_snapshot();
        }
        self.log_new_write_report();
    }

    /// Ближайший момент записи для засыпающего event loop-а.
    pub(crate) fn next_wake_deadline(&self) -> Option<Instant> {
        match &self.persistence {
            Persistence::Writing(state) => state.save_due_at,
            Persistence::InMemoryOnly => None,
        }
    }

    /// Границы жизненного цикла (смена media, suspend): несохранённое уходит на запись сразу.
    /// Неблокирующий: поток записи сам завершит атомарную замену файла.
    pub(crate) fn flush_unsaved_changes(&mut self) {
        let has_unsaved = matches!(
            &self.persistence,
            Persistence::Writing(state) if state.save_due_at.is_some()
        );
        if has_unsaved {
            self.submit_snapshot();
        }
    }

    /// Выход из приложения: последний снимок и ожидание потока записи не дольше `timeout`.
    pub(crate) fn shutdown(&mut self, timeout: Duration) -> TrackMemoryShutdownOutcome {
        let Persistence::Writing(state) = &mut self.persistence else {
            return TrackMemoryShutdownOutcome::NothingToFlush;
        };
        let newest = state.save_due_at.take().and_then(|_| {
            let revision = TrackMemorySaveRevision::new(NonZeroU64::new(state.next_revision)?);
            state.next_revision = state.next_revision.checked_add(1)?;
            Some(TrackMemoryWriteSnapshot::new(revision, self.memory.clone()))
        });
        match state.writer.shutdown(newest, timeout) {
            TrackMemoryWriterShutdownOutcome::Completed { final_report } => {
                TrackMemoryShutdownOutcome::Completed(final_report)
            }
            TrackMemoryWriterShutdownOutcome::TimedOut => TrackMemoryShutdownOutcome::TimedOut,
            TrackMemoryWriterShutdownOutcome::WorkerUnavailable
            | TrackMemoryWriterShutdownOutcome::AlreadyCompleted => {
                TrackMemoryShutdownOutcome::WriterUnavailable
            }
        }
    }

    /// Запись текущего видео, если media совпадает с тем, для которого действует память.
    fn current_for(&self, media_instance_id: MediaInstanceId) -> Option<CurrentVideo> {
        self.current
            .filter(|current| current.media_instance_id == media_instance_id)
    }

    fn after_remember(&mut self, outcome: RememberOutcome, now: Instant) -> RecordOutcome {
        match outcome {
            RememberOutcome::Unchanged => RecordOutcome::Unchanged,
            RememberOutcome::Stored { .. } => {
                self.mark_unsaved(now);
                RecordOutcome::Stored
            }
        }
    }

    /// Отмечает несохранённые изменения и сдвигает срок записи на паузу покоя вперёд.
    fn mark_unsaved(&mut self, now: Instant) {
        if let Persistence::Writing(state) = &mut self.persistence {
            state.save_due_at = Some(now + TRACK_MEMORY_SAVE_DEBOUNCE);
        }
    }

    /// Кладёт снимок памяти в слот фонового потока.
    fn submit_snapshot(&mut self) {
        let Persistence::Writing(state) = &mut self.persistence else {
            return;
        };
        let Some(revision) = NonZeroU64::new(state.next_revision).map(TrackMemorySaveRevision::new)
        else {
            return;
        };
        let snapshot = TrackMemoryWriteSnapshot::new(revision, self.memory.clone());
        match state.writer.submit(snapshot) {
            playlist_state::TrackMemorySubmitOutcome::Accepted => {
                state.next_revision = state.next_revision.saturating_add(1);
                state.save_due_at = None;
            }
            playlist_state::TrackMemorySubmitOutcome::SameOrOlderRevision => {
                // Ревизии выдаёт только этот владелец и строго по возрастанию.
                tracing::error!("Track memory writer rejected a revision as stale");
                state.next_revision = state.next_revision.saturating_add(1);
            }
            playlist_state::TrackMemorySubmitOutcome::Disconnected => {
                tracing::warn!("Track memory writer stopped; choices are kept in memory only");
                self.persistence = Persistence::InMemoryOnly;
            }
        }
    }

    /// Сообщает в лог (без путей) о новой попытке записи, если она не удалась.
    fn log_new_write_report(&mut self) {
        let Persistence::Writing(state) = &mut self.persistence else {
            return;
        };
        let Some(report) = state.writer.latest_report() else {
            return;
        };
        if state.last_logged_revision == Some(report.revision) {
            return;
        }
        state.last_logged_revision = Some(report.revision);
        match report.outcome {
            playlist_state::AtomicWriteOutcome::Durable => {
                tracing::debug!(revision = report.revision.get(), "Track memory saved");
            }
            playlist_state::AtomicWriteOutcome::ReplacedDurabilityUnconfirmed(cause) => {
                tracing::warn!(?cause, "Track memory saved but durability is unconfirmed");
            }
            playlist_state::AtomicWriteOutcome::NotReplaced(failure) => {
                tracing::warn!(?failure, "Track memory could not be saved");
            }
        }
    }

    /// Текущая ёмкость (для тестов и диагностики).
    #[cfg(test)]
    pub(crate) fn capacity(&self) -> TrackMemoryCapacity {
        self.memory.capacity()
    }

    /// Число запомненных видео (для тестов).
    #[cfg(test)]
    pub(crate) fn remembered_video_count(&self) -> usize {
        self.memory.len()
    }

    /// Как память переживает сессию (для тестов).
    #[cfg(test)]
    pub(crate) fn is_writing_to_disk(&self) -> bool {
        matches!(self.persistence, Persistence::Writing(_))
    }
}
