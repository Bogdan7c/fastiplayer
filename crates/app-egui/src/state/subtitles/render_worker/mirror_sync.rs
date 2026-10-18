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
//! - событий больше -> `Update` с ТОЛЬКО новыми событиями (id >= отправленного числа, в порядке
//!   id, поэтому id в зеркале совпадают с id у UI);
//! - то же число событий, но другая ревизия таймлайна -> какие-то концы укорочены (открытая
//!   реплика закрыта преемником): `Update` с укорочениями `(id, новый конец)` из журнала
//!   таймлайна (`shortenings_since`). Укорочение и дописывание за один кадр едут одной командой;
//! - журнал укорочений не помнит нужной ревизии -> страховка: полная пересинхронизация `Replace`;
//! - событий меньше (чего быть не должно) -> та же страховка.
//!
//! Укороченное событие не пересоздаётся: его id и `CueKey` те же, поэтому уже нарисованная
//! картинка, показ которой не зависит от потерянного отрезка, остаётся годной (без мигания).
//!
//! Каждое изменение получает возрастающий номер (`serial`): поток помечает им нарисованные
//! картинки, и UI по номеру отличает картинки, нарисованные ДО дописывания.
//!
//! Недостаток: зеркало обновляется на следующем кадре после изменения, а не мгновенно; для
//! рисования это не важно (кадры идут постоянно, на паузе дописывание будит UI само).

use subtitle_core::{EndShortening, SubtitleEvent};
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
    /// Ревизия таймлайна на момент последней сверки (см. `SubtitleTimeline::revision`).
    revision: u64,
}

/// Итог сверки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum SyncOutcome {
    /// Зеркало не менялось.
    Unchanged,
    /// Дорожка отправлена целиком (первая отправка или полная пересинхронизация): всё, что
    /// поток нарисовал для неё раньше, недействительно.
    Replaced { serial: u64 },
    /// Отправлена узкая дельта: после `serial` у потока новые реплики и/или укороченные концы.
    Updated {
        serial: u64,
        /// Время дописанных реплик: недействительно всё, что задевает этот отрезок.
        appended: Option<TimeSpan>,
        /// Отрезок, потерянный укороченными событиями: нарисованное до его начала остаётся
        /// верным (срок обрезается), нарисованное внутри недействительно.
        shortened: Option<TimeSpan>,
    },
}

/// Учёт отправленного потоку.
#[derive(Debug, Default)]
pub(in crate::state::subtitles) struct MirrorSync {
    sent: [Option<SentTrack>; TrackRole::COUNT],
    /// Счётчик изменений зеркал: каждая команда `Replace`/`Update` получает свой номер.
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
        let revision = track.timeline().revision();
        let sent = self.sent[role.index()];
        match sent {
            Some(sent) if sent.source == track.source_id() => {
                if event_count == sent.event_count && revision == sent.revision {
                    return Ok(SyncOutcome::Unchanged);
                }
                if event_count < sent.event_count {
                    // Страховка: дорожка «усохла», доверять дельте нельзя.
                    return self.replace(role, track, send);
                }
                self.send_update(role, track, sent, send)
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

    /// Отправляет узкую дельту относительно `sent`: укорочения из журнала и дописанные реплики.
    fn send_update(
        &mut self,
        role: TrackRole,
        track: &ActiveSubtitleTrack,
        sent: SentTrack,
        send: &mut dyn FnMut(WorkerCommand) -> Result<(), WorkerLost>,
    ) -> Result<SyncOutcome, WorkerLost> {
        let Ok(history) = track.timeline().shortenings_since(sent.revision) else {
            // Журнал укорочений переполнился: дельту собрать нельзя, страховка - полная копия.
            return self.replace(role, track, send);
        };
        // Укорочения событий, добавленных ПОСЛЕ прошлой сверки, не нужны: такие события едут в
        // `appended` сразу с актуальным концом.
        let shortenings: Vec<EndShortening> = history
            .filter(|shortening| {
                usize::try_from(shortening.id.get()).is_ok_and(|index| index < sent.event_count)
            })
            .collect();
        let shortened = shortenings
            .iter()
            .map(|shortening| TimeSpan::new(shortening.new_end, shortening.previous_end))
            .reduce(TimeSpan::union);
        let (appended, appended_span) = events_since(track, sent.event_count);
        self.sent[role.index()] = Some(SentTrack {
            source: track.source_id(),
            event_count: track.timeline().len(),
            revision: track.timeline().revision(),
        });
        if shortenings.is_empty() && appended_span.is_none() {
            // Ревизия выросла, но зеркалу передавать нечего (например, укорочены только
            // события, которые само зеркало ещё не видело).
            return Ok(SyncOutcome::Unchanged);
        }
        let serial = self.next_serial();
        send(WorkerCommand::Track(TrackCommand::Update {
            serial,
            source: track.source_id(),
            shortenings,
            appended,
        }))?;
        Ok(SyncOutcome::Updated {
            serial,
            appended: appended_span,
            shortened,
        })
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
            revision: track.timeline().revision(),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::subtitles::render_worker::commands::TrackCommand;
    use subtitle_core::{
        CuePlacement, SpanStyle, StyledText, SubtitleContent, SubtitleEventId, SubtitleLayer,
        SubtitleSyncOffset, SubtitleTextCue, SubtitleTime, SubtitleTimeline, TextSpan,
    };

    fn ms(value: i64) -> SubtitleTime {
        SubtitleTime::from_millis(value)
    }

    fn event(start_ms: i64, end_ms: i64, text: &str) -> SubtitleEvent {
        SubtitleEvent::new(
            ms(start_ms),
            ms(end_ms),
            SubtitleLayer::DEFAULT,
            SubtitleContent::Text(SubtitleTextCue {
                text: StyledText::new(vec![TextSpan::text(text, SpanStyle::default())]),
                placement: CuePlacement::default(),
            }),
        )
        .expect("valid interval")
    }

    fn track_with(events: impl IntoIterator<Item = SubtitleEvent>) -> ActiveSubtitleTrack {
        let (timeline, _) = SubtitleTimeline::from_events(events);
        ActiveSubtitleTrack::new(
            CueSourceId::new(1),
            timeline,
            SubtitleSyncOffset::ZERO,
            None,
        )
    }

    /// Синхронизирует и возвращает исход вместе со всеми отправленными командами.
    fn sync_once(
        mirror: &mut MirrorSync,
        track: &ActiveSubtitleTrack,
    ) -> (SyncOutcome, Vec<TrackCommand>) {
        let mut sent = Vec::new();
        let outcome = mirror
            .sync(TrackRole::Display, track, &mut |command| {
                if let WorkerCommand::Track(track_command) = command {
                    sent.push(track_command);
                }
                Ok(())
            })
            .expect("the fake link never fails");
        (outcome, sent)
    }

    fn id(raw: usize) -> SubtitleEventId {
        let (timeline, _) = SubtitleTimeline::from_events((0..=raw).map(|index| {
            event(
                index as i64 * 10,
                index as i64 * 10 + 5,
                &format!("e{index}"),
            )
        }));
        timeline
            .iter()
            .map(|(id, _)| id)
            .find(|id| id.get() as usize == raw)
            .expect("id exists")
    }

    #[test]
    fn first_sync_replaces_and_a_repeat_without_changes_sends_nothing() {
        let track = track_with([event(0, 1_000, "open")]);
        let mut mirror = MirrorSync::default();
        let (first, commands) = sync_once(&mut mirror, &track);
        assert!(matches!(first, SyncOutcome::Replaced { .. }));
        assert!(matches!(commands[..], [TrackCommand::Replace { .. }]));
        let (second, commands) = sync_once(&mut mirror, &track);
        assert_eq!(second, SyncOutcome::Unchanged);
        assert!(commands.is_empty());
    }

    #[test]
    fn shortening_is_sent_as_a_narrow_update_without_replace_and_reports_the_lost_span() {
        let mut track = track_with([event(0, 1_000, "open"), event(10, 20, "other")]);
        let mut mirror = MirrorSync::default();
        sync_once(&mut mirror, &track);

        track
            .shorten_event_end(id(0), ms(400))
            .expect("shorten the open event");
        let (outcome, commands) = sync_once(&mut mirror, &track);

        let SyncOutcome::Updated {
            appended,
            shortened,
            ..
        } = outcome
        else {
            panic!("expected Updated, got {outcome:?}");
        };
        assert_eq!(appended, None);
        assert_eq!(shortened, Some(TimeSpan::new(ms(400), ms(1_000))));
        let [
            TrackCommand::Update {
                shortenings,
                appended: appended_events,
                ..
            },
        ] = &commands[..]
        else {
            panic!("expected a single Update, got {} commands", commands.len());
        };
        assert!(appended_events.is_empty());
        assert_eq!(shortenings.len(), 1);
        assert_eq!(shortenings[0].id, id(0));
        assert_eq!(shortenings[0].new_end, ms(400));
        assert_eq!(shortenings[0].previous_end, ms(1_000));
        // Повторная сверка без изменений молчит.
        assert_eq!(sync_once(&mut mirror, &track).0, SyncOutcome::Unchanged);
    }

    #[test]
    fn shortening_and_append_in_one_sync_travel_in_one_update() {
        let mut track = track_with([event(0, 1_000, "open")]);
        let mut mirror = MirrorSync::default();
        sync_once(&mut mirror, &track);

        track.shorten_event_end(id(0), ms(300)).expect("shorten");
        track.append_events([event(300, 2_000, "next")]);
        let (outcome, commands) = sync_once(&mut mirror, &track);

        assert!(matches!(
            outcome,
            SyncOutcome::Updated {
                appended: Some(_),
                shortened: Some(_),
                ..
            }
        ));
        let [
            TrackCommand::Update {
                shortenings,
                appended,
                ..
            },
        ] = &commands[..]
        else {
            panic!("expected a single Update");
        };
        assert_eq!(shortenings.len(), 1);
        assert_eq!(appended.len(), 1);
        assert_eq!(appended[0].start(), ms(300));
    }

    #[test]
    fn shortening_of_an_event_the_mirror_has_not_seen_travels_only_inside_the_appended_event() {
        let mut track = track_with([event(0, 100, "first")]);
        let mut mirror = MirrorSync::default();
        sync_once(&mut mirror, &track);

        track.append_events([event(200, 9_000, "late open")]);
        let late = id(1);
        track.shorten_event_end(late, ms(500)).expect("shorten");
        let (outcome, commands) = sync_once(&mut mirror, &track);

        let SyncOutcome::Updated { shortened, .. } = outcome else {
            panic!("expected Updated, got {outcome:?}");
        };
        assert_eq!(shortened, None, "the mirror never held the long version");
        let [
            TrackCommand::Update {
                shortenings,
                appended,
                ..
            },
        ] = &commands[..]
        else {
            panic!("expected a single Update");
        };
        assert!(shortenings.is_empty());
        assert_eq!(appended[0].end(), ms(500), "appended with the final end");
    }

    #[test]
    fn lost_shortening_history_falls_back_to_a_full_replace() {
        let long_end = 10_000_000;
        let mut track = track_with([event(0, long_end, "open")]);
        let mut mirror = MirrorSync::default();
        sync_once(&mut mirror, &track);

        // Больше укорочений, чем помнит журнал, между двумя сверками.
        for step in 1..=2_000 {
            track
                .shorten_event_end(id(0), ms(long_end - step))
                .expect("shorten");
        }
        let (outcome, commands) = sync_once(&mut mirror, &track);

        assert!(matches!(outcome, SyncOutcome::Replaced { .. }));
        assert!(matches!(commands[..], [TrackCommand::Replace { .. }]));
    }

    #[test]
    fn serials_grow_across_updates_and_never_repeat() {
        let mut track = track_with([event(0, 1_000, "open")]);
        let mut mirror = MirrorSync::default();
        let (SyncOutcome::Replaced { serial: first }, _) = sync_once(&mut mirror, &track) else {
            panic!("first sync must replace");
        };
        track.shorten_event_end(id(0), ms(900)).expect("shorten");
        let (SyncOutcome::Updated { serial: second, .. }, _) = sync_once(&mut mirror, &track)
        else {
            panic!("expected Updated");
        };
        assert!(second > first);
    }
}
