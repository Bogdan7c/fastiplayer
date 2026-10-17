//! UI-сторона синхронизации зеркал дорожек с потоком-рисовальщиком.
//!
//! # Как выбрана схема (решение)
//! Таймлайн дорожки меняют ровно три пути: загрузка файла (дорожка создаётся целиком, у неё
//! новый `CueSourceId`), дописывание реплик встроенной дорожки (`append_events` в
//! `apply_batches`, id событий = порядок вставки) и `restart_with_source` (новый `CueSourceId`
//! и пустой таймлайн). Значит состояние дорожки полностью описывается парой
//! «`CueSourceId` + число событий». Вместо того чтобы пробрасывать команды из каждого места,
//! презентатор на каждом кадре сверяет эту пару с тем, что уже отправлено потоку:
//! - другой источник -> `Forget` старого и `Replace` новым (полная копия);
//! - то же число событий -> ничего (путь обычного кадра: два сравнения чисел, без аллокаций);
//! - событий больше -> `Append` ТОЛЬКО новых событий (id >= отправленного числа, в порядке id,
//!   поэтому id в зеркале совпадают с id у UI);
//! - событий меньше (чего быть не должно) -> страховка: полная пересинхронизация `Replace`.
//!
//! Каждое изменение получает возрастающий номер (`serial`): поток помечает им нарисованные
//! картинки, и UI по номеру отличает картинки, нарисованные ДО дописывания.
//!
//! Недостаток: зеркало обновляется на следующем кадре после изменения, а не мгновенно; для
//! рисования это не важно (кадры идут постоянно, на паузе дописывание будит UI само).

use subtitle_core::SubtitleEvent;
use subtitle_raster::CueSourceId;

use super::commands::{TrackCommand, WorkerCommand};
use super::handle::WorkerLost;
use super::validity::TimeSpan;
use crate::state::subtitles::active_track::ActiveSubtitleTrack;

/// Роль дорожки: у каждой роли своя ячейка зеркала (так старая дорожка роли не копится в
/// потоке).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum TrackRole {
    /// Дорожка, выбранная пользователем.
    Display,
    /// Образец режима правки.
    Sample,
}

impl TrackRole {
    const COUNT: usize = 2;

    fn index(self) -> usize {
        match self {
            Self::Display => 0,
            Self::Sample => 1,
        }
    }
}

/// Что отправлено потоку про дорожку роли.
#[derive(Debug, Clone, Copy)]
struct SentTrack {
    source: CueSourceId,
    event_count: usize,
}

/// Итог сверки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum SyncOutcome {
    /// Зеркало не менялось.
    Unchanged,
    /// Дорожка отправлена целиком (первая отправка или полная пересинхронизация): всё, что
    /// поток нарисовал для неё раньше, недействительно.
    Replaced { serial: u64 },
    /// Дописаны реплики, покрывающие `span`: недействительно то, что задевает этот отрезок.
    Appended { serial: u64, span: TimeSpan },
}

/// Учёт отправленного потоку.
#[derive(Debug, Default)]
pub(in crate::state::subtitles) struct MirrorSync {
    sent: [Option<SentTrack>; TrackRole::COUNT],
    /// Счётчик изменений зеркал: каждая команда `Replace`/`Append` получает свой номер.
    last_serial: u64,
}

impl MirrorSync {
    fn next_serial(&mut self) -> u64 {
        self.last_serial += 1;
        self.last_serial
    }

    /// Сверяет дорожку роли `role` с отправленным и шлёт нужные команды через `send`.
    pub(in crate::state::subtitles) fn sync(
        &mut self,
        role: TrackRole,
        track: &ActiveSubtitleTrack,
        send: &mut dyn FnMut(WorkerCommand) -> Result<(), WorkerLost>,
    ) -> Result<SyncOutcome, WorkerLost> {
        let event_count = track.timeline().len();
        let sent = self.sent[role.index()];
        match sent {
            Some(sent) if sent.source == track.source_id() => {
                if event_count == sent.event_count {
                    return Ok(SyncOutcome::Unchanged);
                }
                if event_count < sent.event_count {
                    // Страховка: дорожка «усохла», доверять дельте нельзя.
                    return self.replace(role, track, send);
                }
                let (events, span) = events_since(track, sent.event_count);
                self.sent[role.index()] = Some(SentTrack {
                    source: track.source_id(),
                    event_count,
                });
                let Some(span) = span else {
                    return Ok(SyncOutcome::Unchanged);
                };
                let serial = self.next_serial();
                send(WorkerCommand::Track(TrackCommand::Append {
                    serial,
                    source: track.source_id(),
                    events,
                }))?;
                Ok(SyncOutcome::Appended { serial, span })
            }
            previous => {
                if let Some(previous) = previous {
                    send(WorkerCommand::Track(TrackCommand::Forget {
                        source: previous.source,
                    }))?;
                }
                self.replace(role, track, send)
            }
        }
    }

    /// Отправляет дорожку целиком.
    fn replace(
        &mut self,
        role: TrackRole,
        track: &ActiveSubtitleTrack,
        send: &mut dyn FnMut(WorkerCommand) -> Result<(), WorkerLost>,
    ) -> Result<SyncOutcome, WorkerLost> {
        let serial = self.next_serial();
        send(WorkerCommand::Track(TrackCommand::Replace {
            serial,
            source: track.source_id(),
            language: track.language().cloned(),
            timeline: track.timeline().clone(),
        }))?;
        self.sent[role.index()] = Some(SentTrack {
            source: track.source_id(),
            event_count: track.timeline().len(),
        });
        Ok(SyncOutcome::Replaced { serial })
    }

    /// Просит поток забыть все зеркала (media исчезло) и забывает отправленное.
    pub(in crate::state::subtitles) fn release_all(
        &mut self,
        send: &mut dyn FnMut(WorkerCommand) -> Result<(), WorkerLost>,
    ) -> Result<(), WorkerLost> {
        for slot in &mut self.sent {
            if let Some(sent) = slot.take() {
                send(WorkerCommand::Track(TrackCommand::Forget {
                    source: sent.source,
                }))?;
            }
        }
        Ok(())
    }

    /// Забывает отправленное без команд (поток новый или мёртв). Счётчик изменений не
    /// сбрасывается: номера не должны повторяться.
    pub(in crate::state::subtitles) fn reset(&mut self) {
        self.sent = [None; TrackRole::COUNT];
    }
}

/// События с id >= `already_sent` в порядке id и отрезок времени, который они занимают.
/// Берутся напрямую по диапазону id: стоимость пропорциональна числу новых событий.
fn events_since(
    track: &ActiveSubtitleTrack,
    already_sent: usize,
) -> (Vec<SubtitleEvent>, Option<TimeSpan>) {
    let mut events = Vec::new();
    let mut span: Option<TimeSpan> = None;
    for (_, event) in track.timeline().events_from_index(already_sent) {
        let event_span = TimeSpan::new(event.start(), event.end());
        span = Some(span.map_or(event_span, |current| current.union(event_span)));
        events.push(event.clone());
    }
    (events, span)
}
