//! Дорожки потока (HLS-рендиции) у владельца субтитров: список из источника, жизненный цикл
//! фоновой ленты, приём реплик, история живой трансляции и уведомление о недоступности.
//!
//! # Архитектура
//! - **Источник и список.** Источник (`StreamSubtitleSource`) приходит вместе с media. Каждая
//!   рендиция становится дорожкой списка сразу `Ready` с пустым таймлайном (как встроенная):
//!   выбор мгновенный, реплики дописывает лента. Подпись, автовыбор и память - `stream_track`.
//! - **Лента живёт ровно пока дорожка показывается.** [`StreamTracks::reconcile`] сравнивает
//!   «какую дорожку потока надо показывать» (выбранную при `Shown` либо принудительную при
//!   `Hidden`) с работающей лентой: смена дорожки, выключение, смена media и сброс отменяют
//!   ленту, выбор запускает новую. Лента принадлежит `MediaSubtitles`, поэтому смена media
//!   уничтожает её вместе с очередью сообщений: чужие реплики старого media прийти не могут.
//! - **Позиция.** `observe_stream_playback` пишет в ячейку ленты ВРЕМЯ ПОИСКА (позиция
//!   плеера минус сдвиг синхронизации): продвижение без пробуждения, прыжок с пробуждением
//!   (seek и смена сдвига). Подробности - `stream_position`.
//! - **Реплики.** Приходят пачками на шкале плеера и дописываются в таймлайн (дубликаты после
//!   повторной загрузки отбрасывает таймлайн). Живая трансляция бесконечна, поэтому старые
//!   реплики вытесняются пересборкой таймлайна под новым источником, а не отбрасыванием новых.
//!   Пересборка идёт только когда давно закончившихся реплик не меньше оставшихся (и не
//!   меньше [`EVICTION_MIN_BATCH`]): память ограничена вдвое от истории, а стоимость
//!   амортизируется, пересборки каждый кадр нет. У VOD вытеснения нет: лента не присылает
//!   обработанные сегменты повторно, вытесненное было бы потеряно.
//! - **Сбой.** `TrackUnavailable` приходит от ленты один раз, после чего она остановлена.
//!   Владелец запоминает дорожку как недоступную (ленту для неё больше не запускает), кладёт
//!   одно уведомление и оставляет дорожку выбираемой: повторный выбор пользователем сбрасывает
//!   пометку и запускает новую ленту. Автоповторов нет. Пропущенные сегменты - только в лог
//!   (без адресов).
//!
//! # Инварианты
//! - Видео субтитры не ждут: всё здесь неблокирующее, закрытие ленты не ждёт её поток.
//! - Адреса и параметры запросов не попадают в лог, `Debug` и уведомления.

use std::sync::Arc;
use std::time::Duration;

use media_source_open::stream_subtitles::{
    StreamPlaybackPosition, StreamSegmentSkipReason, StreamSubtitleFeed, StreamSubtitleFeedConfig,
    StreamSubtitleMessage, StreamSubtitleSource, StreamSubtitleTrackId,
};
use subtitle_core::{
    SubtitleEvent, SubtitleSyncOffset, SubtitleTime, SubtitleTimeline, SubtitleTrackId,
};

use super::active_track::{ActiveSubtitleTrack, subtitle_time_from_pts};
use super::media_state::{DiscoveryState, DisplayMode, MediaSubtitles};
use super::owner_memory::PendingRememberedChoice;
use super::stream_position::{
    PositionUpdate, StreamPlaybackObservation, StreamPositionTracker, StreamTimelineKind,
};
use super::stream_track::StreamTrackDescription;
use super::track_list::TrackLoadState;
use super::web_track::WebSubtitleSource;
use super::{CurrentMediaForSubtitles, EmbeddedUpdate, SubtitleOwner};
use crate::app_wake::AppWakePort;

/// Минимум давно закончившихся реплик для пересборки таймлайна (внутренний инвариант:
/// пересборка меняет источник реплик и перерисовывает зеркало потока-рисовальщика целиком).
const EVICTION_MIN_BATCH: usize = 16;

/// Как часто проверять историю по ходу воспроизведения: позиция должна уйти вперёд на столько
/// (проверка считает реплики дорожки, поэтому не на каждом кадре).
const EVICTION_CHECK_STEP: Duration = Duration::from_secs(5);

/// Что пользователь должен узнать о субтитрах потока.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamSubtitleNotice {
    /// Дорожка потока сейчас недоступна (сегменты не грузятся); выбор повторяет попытку.
    TrackUnavailable,
}

/// Счётчики ленты (диагностика и тесты).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct StreamFeedCounters {
    /// Реплики, добавленные в таймлайн.
    pub(super) cues_added: u64,
    /// Дубликаты реплик (повторная загрузка тех же сегментов).
    pub(super) duplicate_cues: u64,
    /// Реплики, не поместившиеся в `max_cues_per_file`.
    pub(super) cues_over_capacity: u64,
    /// Пропущенные сегменты (сбой, вытеснение, уход из плейлиста).
    pub(super) segments_skipped: u64,
    /// Реплики, вытесненные историей живой трансляции.
    pub(super) cues_evicted: u64,
}

/// Работающая лента и дорожка списка, которой она принадлежит.
struct RunningStreamFeed {
    track: SubtitleTrackId,
    feed: StreamSubtitleFeed,
}

/// Какую ленту надо иметь сейчас.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct WantedStreamFeed {
    /// Дорожка списка владельца.
    pub(super) track: SubtitleTrackId,
    /// Дорожка в источнике потока.
    pub(super) feed_track: StreamSubtitleTrackId,
}

/// Состояние потоковых субтитров одного media (живёт и умирает вместе с `MediaSubtitles`).
pub(super) struct StreamTracks {
    /// Источник потока; `None` - у media потоковых дорожек нет.
    source: Option<StreamSubtitleSource>,
    /// Порт пробуждения ленты с собственным флагом (его очищает только чтение её очереди).
    wake_port: Option<AppWakePort>,
    /// Ячейка позиции, общая с лентой.
    position: StreamPlaybackPosition,
    /// Отличает продвижение позиции от прыжка.
    position_tracker: StreamPositionTracker,
    /// Последняя позиция плеера (шкала плеера); `None` - ещё не наблюдали.
    last_position: Option<Duration>,
    /// Последнее время поиска реплик (позиция минус сдвиг), которое видит лента.
    last_lookup_position: Option<Duration>,
    /// Сдвиг синхронизации, с которым опубликовано время поиска.
    published_offset: Option<SubtitleSyncOffset>,
    /// Был ли уже опубликован хоть один сдвиг (первое наблюдение не считается его сменой).
    published_offset_known: bool,
    /// VOD или live (по последнему наблюдению).
    timeline: StreamTimelineKind,
    /// Работающая лента (одна: показывается одна дорожка).
    running: Option<RunningStreamFeed>,
    /// Дорожки, чья лента сообщила о недоступности и не запускается до повторного выбора.
    unavailable: Vec<SubtitleTrackId>,
    /// Позиция последней проверки истории (для ограничения частоты).
    last_eviction_check: Option<SubtitleTime>,
    /// Уведомления, ещё не показанные пользователю.
    notices: Vec<StreamSubtitleNotice>,
    counters: StreamFeedCounters,
}

impl Default for StreamTracks {
    fn default() -> Self {
        Self {
            source: None,
            wake_port: None,
            position: StreamPlaybackPosition::new(Duration::ZERO),
            position_tracker: StreamPositionTracker::default(),
            last_position: None,
            last_lookup_position: None,
            published_offset: None,
            published_offset_known: false,
            timeline: StreamTimelineKind::Vod,
            running: None,
            unavailable: Vec::new(),
            last_eviction_check: None,
            notices: Vec::new(),
            counters: StreamFeedCounters::default(),
        }
    }
}

impl StreamTracks {
    /// Запоминает источник потока media и порт, которым лента будит интерфейс.
    fn install_source(&mut self, source: StreamSubtitleSource, wake_port: AppWakePort) {
        self.source = Some(source);
        self.wake_port = Some(wake_port);
    }

    /// Пользователь выбрал дорожку: прежняя недоступность снимается, следующая сверка запустит
    /// новую ленту (единственный путь повторной попытки).
    pub(super) fn note_user_selected(&mut self, track: SubtitleTrackId) {
        self.unavailable.retain(|known| *known != track);
    }

    /// Дорожка признана недоступной: ленты для неё нет, пользователю уходит одно уведомление.
    fn mark_unavailable(&mut self, track: SubtitleTrackId) {
        if self
            .running
            .as_ref()
            .is_some_and(|running| running.track == track)
        {
            self.running = None;
        }
        if !self.unavailable.contains(&track) {
            self.unavailable.push(track);
        }
        self.notices.push(StreamSubtitleNotice::TrackUnavailable);
    }

    /// Приводит работающую ленту к желаемой: ничего не меняет, если лента уже та; иначе
    /// отменяет прежнюю и запускает новую.
    fn reconcile(&mut self, wanted: Option<WantedStreamFeed>, config: StreamSubtitleFeedConfig) {
        let wanted = wanted.filter(|wanted| !self.unavailable.contains(&wanted.track));
        let running_track = self.running.as_ref().map(|running| running.track);
        if running_track == wanted.map(|wanted| wanted.track) {
            return;
        }
        if let Some(previous) = self.running.take() {
            previous.feed.cancel();
        }
        let (Some(wanted), Some(source), Some(wake_port)) =
            (wanted, self.source.as_ref(), self.wake_port.clone())
        else {
            return;
        };
        if let Some(position) = self.last_lookup_position {
            self.position.advance_to(position);
        }
        let wake: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
            // Исход пробуждения не важен: закрытый цикл событий означает завершение приложения.
            let _delivery = wake_port.request_wake();
        });
        match StreamSubtitleFeed::start(
            source,
            wanted.feed_track,
            config,
            self.position.clone(),
            wake,
        ) {
            Ok(feed) => {
                self.running = Some(RunningStreamFeed {
                    track: wanted.track,
                    feed,
                });
            }
            Err(error) => {
                tracing::warn!(%error, "A stream subtitle feed could not be started");
                self.mark_unavailable(wanted.track);
            }
        }
    }

    /// Записывает наблюдение: позиция ленты, шкала и (для проверки истории) последняя позиция.
    ///
    /// Ленте отдаётся ВРЕМЯ ПОИСКА (позиция плеера минус сдвиг синхронизации, не меньше нуля):
    /// реплики лежат на шкале плеера, а кадр ищет их по времени поиска, значит окно загрузки
    /// должно строиться вокруг него. Смена сдвига (Z/X, сброс, восстановленный из памяти) —
    /// прыжок, как seek: окно нужно пересчитать.
    fn observe(
        &mut self,
        observation: &StreamPlaybackObservation,
        sync_offset: SubtitleSyncOffset,
    ) {
        self.last_position = Some(observation.position);
        self.timeline = observation.timeline;
        let lookup_of = |position: Duration| lookup_position(position, sync_offset);
        let lookup_observation = StreamPlaybackObservation {
            position: lookup_of(observation.position),
            seek_target: observation.seek_target.map(lookup_of),
            timeline: observation.timeline,
        };
        self.last_lookup_position = Some(lookup_observation.position);
        let offset_changed = self.published_offset.replace(sync_offset) != Some(sync_offset)
            && self.published_offset_known;
        self.published_offset_known = true;
        let update = self.position_tracker.update(&lookup_observation);
        match update {
            _ if offset_changed => self.position.jump_to(lookup_observation.position),
            PositionUpdate::Unchanged => {}
            PositionUpdate::Advance(position) => self.position.advance_to(position),
            PositionUpdate::Jump(position) => self.position.jump_to(position),
        }
    }
}

impl MediaSubtitles {
    /// Лента, которая нужна сейчас: дорожка потока, которую показывают целиком.
    fn wanted_stream_feed(&self) -> Option<WantedStreamFeed> {
        let choice = self.display_choice()?;
        if choice.mode != DisplayMode::AllCues {
            return None;
        }
        let entry = self.tracks.get(choice.track_id)?;
        let description = entry.description.stream()?;
        matches!(entry.load, TrackLoadState::Ready(_)).then(|| WantedStreamFeed {
            track: choice.track_id,
            feed_track: description.feed_track_id(),
        })
    }
}

impl SubtitleOwner {
    /// Подключает к списку media дорожки web (extractor) и потока (HLS) и решает, нужен ли
    /// поиск запомненного файла. Список полный с самого начала, поэтому память выбора и
    /// автовыбор решаются сразу (вызывающий запускает `reselect_automatically`).
    pub(super) fn install_remote_sources(
        &mut self,
        media: &mut MediaSubtitles,
        web: Option<&WebSubtitleSource>,
        stream: Option<StreamSubtitleSource>,
    ) -> DiscoveryState {
        if let Some(web) = web {
            let descriptions = web.descriptions_for_menu(&self.preferred_languages());
            media.install_web_source(web, descriptions);
        }
        if let Some(stream) = stream {
            self.install_stream_source(media, stream);
        }
        // Поиск файлов нужен, только если из памяти ждём файл, который пользователь когда-то
        // добавил сам.
        if media
            .pending_memory
            .as_ref()
            .and_then(PendingRememberedChoice::file_to_check)
            .is_some()
        {
            self.start_discovery(None, media)
        } else {
            DiscoveryState::Finished
        }
    }

    /// Дорожки потока встают в конец списка готовыми (пустой таймлайн, реплики допишет лента).
    fn install_stream_source(&mut self, media: &mut MediaSubtitles, stream: StreamSubtitleSource) {
        for description in StreamTrackDescription::all_of(&stream) {
            let load = TrackLoadState::Ready(ActiveSubtitleTrack::new(
                self.source_ids.allocate(),
                SubtitleTimeline::new(),
                media.sync_offset,
                description.language().cloned(),
            ));
            media.tracks.push_stream_track(description, load);
        }
        media
            .streams
            .install_source(stream, self.wake_port.with_independent_edge());
        // Текстовым дорожкам нужен растеризатор, даже если файлов рядом нет.
        self.request_rasterizer_in_background();
    }

    /// Приводит ленту к тому, что показывается: вызывается при любой смене выбора, видимости,
    /// списка дорожек и перед чтением сообщений.
    pub(super) fn sync_stream_feed(&mut self) {
        let config = self.settings.stream_limits.feed;
        let Some(media) = self.media.as_mut() else {
            return;
        };
        let wanted = media.wanted_stream_feed();
        media.streams.reconcile(wanted, config);
    }

    /// Принимает наблюдение за плеером (каждый кадр UI): двигает позицию ленты и, для live,
    /// проверяет историю реплик.
    pub(super) fn observe_stream_playback(
        &mut self,
        current: CurrentMediaForSubtitles,
        observation: &StreamPlaybackObservation,
    ) -> EmbeddedUpdate {
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        if current.media_instance_id != Some(media.media_instance_id)
            || media.streams.source.is_none()
        {
            return EmbeddedUpdate::Unchanged;
        }
        media.streams.observe(observation, media.sync_offset);
        self.evict_stream_history(HistoryCheck::Throttled)
    }

    /// Опрос ленты по wake-событию. `true` - на экране могла измениться реплика.
    ///
    /// Флаг пробуждения ленты очищается ПРЯМО перед чтением очереди: сообщение, пришедшее
    /// после чтения, поднимет флаг заново и разбудит интерфейс (даже на паузе).
    pub(super) fn poll_stream_feed(&mut self) -> bool {
        if let Some(port) = self
            .media
            .as_ref()
            .and_then(|media| media.streams.wake_port.as_ref())
        {
            port.clear_pending_for_drain();
        }
        self.drain_stream_feed() == EmbeddedUpdate::Changed
    }

    /// Читает сообщения ленты показываемой дорожки без блокировки. `Changed`, если на экране
    /// могла измениться реплика.
    pub(super) fn drain_stream_feed(&mut self) -> EmbeddedUpdate {
        self.sync_stream_feed();
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        let Some(running) = media.streams.running.as_ref() else {
            return EmbeddedUpdate::Unchanged;
        };
        let track = running.track;
        let messages = running.feed.drain_messages();
        if messages.is_empty() {
            return EmbeddedUpdate::Unchanged;
        }
        let mut events: Vec<SubtitleEvent> = Vec::new();
        let mut unavailable = false;
        for message in messages {
            match message {
                StreamSubtitleMessage::Events(batch) => events.extend(batch.events),
                StreamSubtitleMessage::SegmentSkipped { segment, reason } => {
                    media.streams.counters.segments_skipped += 1;
                    log_skipped_segment(segment.value(), reason);
                }
                StreamSubtitleMessage::TrackUnavailable(reason) => {
                    // Адресов в причине нет: только вид сбоя и число сегментов.
                    tracing::warn!(
                        ?reason,
                        "Stream subtitles are unavailable; playback continues"
                    );
                    unavailable = true;
                }
                StreamSubtitleMessage::Recovered => {
                    tracing::debug!("Stream subtitle segments load again");
                }
            }
        }
        let mut update = self.append_stream_events(track, events);
        if unavailable && let Some(media) = self.media.as_mut() {
            media.streams.mark_unavailable(track);
            update = EmbeddedUpdate::Changed;
        }
        update.merge(self.evict_stream_history(HistoryCheck::Forced))
    }

    /// Дописывает пачку реплик в таймлайн дорожки с учётом общего потолка числа реплик.
    fn append_stream_events(
        &mut self,
        track_id: SubtitleTrackId,
        mut events: Vec<SubtitleEvent>,
    ) -> EmbeddedUpdate {
        if events.is_empty() {
            return EmbeddedUpdate::Unchanged;
        }
        let max_cues = self.settings.embedded_limits.max_cues_per_track;
        // Живая лента бесконечна: у потолка сначала освобождаем место закончившимися репликами.
        let evicted_update = self.make_room_on_live_track(track_id, events.len(), max_cues);
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        let displayed = media.display_track_id();
        let Some(TrackLoadState::Ready(track)) =
            media.tracks.get_mut(track_id).map(|entry| &mut entry.load)
        else {
            return EmbeddedUpdate::Unchanged;
        };
        let counters = &mut media.streams.counters;
        let room = max_cues.saturating_sub(track.timeline().len());
        if events.len() > room {
            let excess = events.len() - room;
            events.truncate(room);
            if counters.cues_over_capacity == 0 {
                tracing::warn!(
                    limit = max_cues,
                    "A stream subtitle track reached the cue limit; new cues are skipped"
                );
            }
            counters.cues_over_capacity += u64::try_from(excess).unwrap_or(u64::MAX);
        }
        let report = track.append_events(events);
        counters.cues_added += report.inserted as u64;
        counters.duplicate_cues += report.duplicates_ignored as u64;
        counters.cues_over_capacity += report.rejected_over_capacity as u64;
        let appended = if report.inserted > 0 && Some(track_id) == displayed {
            EmbeddedUpdate::Changed
        } else {
            EmbeddedUpdate::Unchanged
        };
        evicted_update.merge(appended)
    }

    /// Живая трансляция у потолка `max_cues`: вытесняет уже закончившиеся реплики (раньше
    /// позиции поиска), НЕ дожидаясь гистерезиса обычного вытеснения.
    ///
    /// Корень проблемы: обычное вытеснение ждёт, пока мёртвых реплик станет не меньше живых, а
    /// потолок мог быть достигнут раньше - тогда новые реплики отбрасывались бы навсегда. Живые
    /// и будущие реплики не трогаем (их потерять нельзя): если после вытеснения места всё
    /// равно не хватает, лишние входящие отбрасываются и считаются в `cues_over_capacity`.
    /// У VOD ничего не делает: там история не вытесняется.
    fn make_room_on_live_track(
        &mut self,
        track_id: SubtitleTrackId,
        incoming: usize,
        max_cues: usize,
    ) -> EmbeddedUpdate {
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        let (StreamTimelineKind::Live, Some(position)) =
            (media.streams.timeline, media.streams.last_position)
        else {
            return EmbeddedUpdate::Unchanged;
        };
        let lookup_time = media
            .sync_offset
            .lookup_time_for(subtitle_time_from_pts(position));
        let Some(TrackLoadState::Ready(track)) =
            media.tracks.get_mut(track_id).map(|entry| &mut entry.load)
        else {
            return EmbeddedUpdate::Unchanged;
        };
        if track.timeline().len().saturating_add(incoming) <= max_cues {
            return EmbeddedUpdate::Unchanged;
        }
        if track.count_ended_before(lookup_time) == 0 {
            return EmbeddedUpdate::Unchanged;
        }
        let evicted = track.rebuild_without_ended_before(lookup_time, self.source_ids.allocate());
        media.streams.counters.cues_evicted += u64::try_from(evicted).unwrap_or(u64::MAX);
        tracing::debug!(
            evicted,
            "Ended live stream subtitle cues were dropped at the cue limit"
        );
        // Новый источник реплик: показанная картинка прежнего источника больше не верна.
        self.presenter.forget_shown_overlay();
        EmbeddedUpdate::Changed
    }

    /// Вытесняет реплики, давно оставшиеся позади позиции (только живая трансляция).
    ///
    /// Граница - позиция поиска реплик (с учётом сдвига синхронизации пользователя) минус
    /// история из конфига: реплика, которую пользователь ещё может увидеть, не вытесняется.
    fn evict_stream_history(&mut self, check: HistoryCheck) -> EmbeddedUpdate {
        let retention = self.settings.stream_limits.live_retention;
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        let track_id = media.display_track_id();
        let (StreamTimelineKind::Live, Some(position)) =
            (media.streams.timeline, media.streams.last_position)
        else {
            return EmbeddedUpdate::Unchanged;
        };
        let lookup_time = media
            .sync_offset
            .lookup_time_for(subtitle_time_from_pts(position));
        if check == HistoryCheck::Throttled && !history_check_is_due(&media.streams, lookup_time) {
            return EmbeddedUpdate::Unchanged;
        }
        media.streams.last_eviction_check = Some(lookup_time);
        let Some(track_id) = track_id else {
            return EmbeddedUpdate::Unchanged;
        };
        let cutoff = SubtitleTime::from_micros(
            lookup_time
                .as_micros()
                .saturating_sub(retention.as_micros()),
        );
        let Some(TrackLoadState::Ready(track)) =
            media.tracks.get_mut(track_id).map(|entry| &mut entry.load)
        else {
            return EmbeddedUpdate::Unchanged;
        };
        let ended = track.count_ended_before(cutoff);
        let alive = track.timeline().len() - ended;
        // Вытесняем, когда мёртвых не меньше живых: память не больше двойной истории, а
        // пересборка (новый источник, полное зеркало) случается редко.
        if ended < EVICTION_MIN_BATCH || ended < alive {
            return EmbeddedUpdate::Unchanged;
        }
        let evicted = track.rebuild_without_ended_before(cutoff, self.source_ids.allocate());
        media.streams.counters.cues_evicted += u64::try_from(evicted).unwrap_or(u64::MAX);
        tracing::debug!(evicted, alive, "Old live stream subtitle cues were dropped");
        // Новый источник реплик: показанная картинка прежнего источника больше не верна.
        self.presenter.forget_shown_overlay();
        EmbeddedUpdate::Changed
    }

    /// Уведомления потоковых субтитров, ещё не показанные пользователю (выдаются один раз).
    pub(crate) fn take_stream_notices(&mut self) -> Vec<StreamSubtitleNotice> {
        self.media
            .as_mut()
            .map(|media| std::mem::take(&mut media.streams.notices))
            .unwrap_or_default()
    }

    /// Счётчики ленты для тестов.
    #[cfg(test)]
    pub(super) fn stream_feed_counters(&self) -> StreamFeedCounters {
        self.media
            .as_ref()
            .map(|media| media.streams.counters)
            .unwrap_or_default()
    }

    /// Для тестов: дорожка, для которой сейчас работает лента.
    #[cfg(test)]
    pub(super) fn running_stream_feed_track(&self) -> Option<SubtitleTrackId> {
        self.media
            .as_ref()?
            .streams
            .running
            .as_ref()
            .map(|running| running.track)
    }
}

/// Когда проверять историю.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HistoryCheck {
    /// Не чаще, чем позиция продвинулась на [`EVICTION_CHECK_STEP`] (ход воспроизведения).
    Throttled,
    /// Сейчас же (после приёма пачки реплик).
    Forced,
}

/// Позиция ушла от последней проверки далеко (или проверок ещё не было).
fn history_check_is_due(streams: &StreamTracks, lookup_time: SubtitleTime) -> bool {
    let step = i64::try_from(EVICTION_CHECK_STEP.as_micros()).unwrap_or(i64::MAX);
    streams.last_eviction_check.is_none_or(|last| {
        let moved = lookup_time.as_micros().saturating_sub(last.as_micros());
        // Назад (seek в прошлое) тоже повод проверить: граница сдвинулась.
        moved >= step || moved < 0
    })
}

/// Пишет в лог пропущенный сегмент без адресов: сбой загрузки заметен (`warn`), вытеснение и
/// уход из плейлиста - обычные события живого потока (`debug`).
fn log_skipped_segment(segment: u64, reason: StreamSegmentSkipReason) {
    match reason {
        StreamSegmentSkipReason::Failed(kind) => {
            tracing::warn!(segment, ?kind, "A stream subtitle segment was skipped");
        }
        StreamSegmentSkipReason::PendingOverflow | StreamSegmentSkipReason::LeftPlaylist => {
            tracing::debug!(segment, ?reason, "A stream subtitle segment was skipped");
        }
    }
}

/// Время поиска реплик для позиции плеера: позиция минус сдвиг, не меньше нуля.
fn lookup_position(position: Duration, sync_offset: SubtitleSyncOffset) -> Duration {
    let lookup = sync_offset.lookup_time_for(subtitle_time_from_pts(position));
    Duration::from_micros(u64::try_from(lookup.as_micros()).unwrap_or(0))
}
