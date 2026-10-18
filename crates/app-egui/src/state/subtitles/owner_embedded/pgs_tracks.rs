//! Состояние PGS-дорожек media: декодер, хранилище композиций и связь «композиция -> события
//! шкалы» на каждую дорожку. Чистая логика без `SubtitleOwner` (стык с владельцем — в
//! [`super::pgs_owner`]).
//!
//! # Кто чем владеет
//! - [`PgsTrackDecoder`] превращает блоки Matroska в композиции (картинки + позиции);
//! - [`PgsCompositionStore`] хранит композиции с интервалами показа и сама решает, что
//!   вытеснить по памяти (политика удержания);
//! - [`ActiveSubtitleTrack`] (шкала дорожки) принадлежит списку дорожек. Шкала умеет только
//!   ДОПИСЫВАТЬ события и УКОРАЧИВАТЬ конец, поэтому любое изменение хранилища выражено либо
//!   как «дописать в хвост + укоротить предшественника» ([`TailAppend`]), либо как «пересобрать
//!   шкалу целиком под новым источником» (новый `CueSourceId` = полная замена в потоке-
//!   рисовальщике).
//! - Это состояние помнит, какие события шкалы принадлежат какой композиции, чтобы укоротить
//!   именно их, когда приходит преемник.
//!
//! - Вид «только forced-картинки» ([`ForcedPicturesView`]) — вторая шкала с forced-подмножеством
//!   тех же событий; ведётся теми же операциями в том же месте (`apply_to_timeline`), см.
//!   `forced_view.rs`.
//!
//! # Инварианты
//! - Карта «композиция -> события» всегда описывает ТЕКУЩУЮ шкалу: пересборка и пересоздание
//!   шкалы очищают её и заполняют заново.
//! - Одна пачка пакетов (один вызов приёма трубы) даёт на дорожку не больше одной пересборки:
//!   изменения копятся в [`PgsPendingChanges`], а применяются после всех пакетов.
//! - Состояние создаётся лениво на первом пакете дорожки и живёт вместе с media (или до
//!   пересоздания дорожки в списке).
//! - У PGS нет предела «число реплик на дорожку» (`max_cues_per_file`): память картинной
//!   дорожки ограничивают бюджет показываемой и окно фоновых дорожек (решение владельца), а
//!   число реплик для картинок ничего не говорит о памяти.

mod forced_view;

use std::collections::HashMap;
use std::time::Duration;

use player_core::{SubtitleFeedPacket, SubtitleReadRun};
use subtitle_core::{SubtitleEventId, SubtitleSyncOffset, SubtitleTime, SubtitleTrackId};
use subtitle_pgs::{
    CompositionEnd, CompositionHandle, PgsCompositionStore, PgsDecoderLimits, PgsRetention,
    PgsTrackDecoder, RetentionOutcome, StoreChange, TailAppend,
};
use subtitle_raster::CueSourceId;

pub(in crate::state::subtitles) use self::forced_view::ForcedPicturesView;
use crate::state::subtitles::active_track::{ActiveSubtitleTrack, subtitle_time_from_pts};

/// Роль PGS-дорожки: от нее зависит политика памяти.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum PgsTrackRole {
    /// Дорожка показывается: держит композиции в пределах бюджета памяти.
    Displayed,
    /// Дорожка не показывается: держит только хвост перед позицией воспроизведения.
    Background,
}

/// Счётчики PGS для диагностики и тестов (ничего из содержимого субтитров).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(in crate::state::subtitles) struct PgsFeedCounters {
    /// Новые композиции, принятые в хранилище и ставшие событиями шкалы.
    pub(in crate::state::subtitles) compositions_added: u64,
    /// Композиции, которые уже были в хранилище (повторное чтение после seek).
    pub(in crate::state::subtitles) duplicate_compositions: u64,
    /// Разрывы потока (смена прохода чтения демуксера), по числу затронутых дорожек.
    pub(in crate::state::subtitles) discontinuities: u64,
    /// Пакеты прохода чтения СТАРШЕ уже известного (остатки до seek в трубе): отброшены,
    /// иначе они воссоздали бы устаревшую открытую композицию.
    pub(in crate::state::subtitles) stale_run_packets: u64,
    /// Полные пересборки шкалы (новый источник).
    pub(in crate::state::subtitles) timeline_rebuilds: u64,
    /// Композиции, вытесненные политикой памяти.
    pub(in crate::state::subtitles) evicted_compositions: u64,
    /// Укорочения предшественника, которые шкала не смогла выполнить (рассогласование;
    /// шкала пересобрана из хранилища).
    pub(in crate::state::subtitles) shortening_mismatches: u64,
}

/// Изменения хранилища одной дорожки, накопленные за пачку пакетов и ещё не перенесённые на
/// шкалу.
#[derive(Debug, Default)]
pub(in crate::state::subtitles) struct PgsPendingChanges {
    /// Дописывания в хвост по порядку поступления.
    appended: Vec<TailAppend>,
    /// Шкалу нужно пересобрать целиком (дописывания тогда не нужны: хранилище — истина).
    rebuild_required: bool,
}

impl PgsPendingChanges {
    /// Есть ли что переносить на шкалу.
    pub(in crate::state::subtitles) fn is_empty(&self) -> bool {
        self.appended.is_empty() && !self.rebuild_required
    }
}

/// Накопленные изменения по дорожкам за одну пачку сообщений трубы.
pub(in crate::state::subtitles) type PgsBatches = HashMap<SubtitleTrackId, PgsPendingChanges>;

/// Как шкала приняла накопленные изменения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum TimelineChange {
    /// Шкала не менялась.
    Untouched,
    /// События дописаны/укорочены на месте (идентичность источника прежняя).
    Edited,
    /// Шкала пересоздана под новым источником.
    Rebuilt,
}

/// PGS-дорожка: декодер, хранилище и связь с шкалой.
pub(in crate::state::subtitles) struct PgsTrackState {
    decoder: PgsTrackDecoder,
    store: PgsCompositionStore,
    /// События ТЕКУЩЕЙ шкалы каждой композиции (у «очистки» список пуст).
    timeline_events: HashMap<CompositionHandle, Vec<SubtitleEventId>>,
    /// Forced-подмножество шкалы: его показывают при выключенных субтитрах без forced-дорожки.
    forced_pictures: ForcedPicturesView,
}

impl PgsTrackState {
    fn new(limits: PgsDecoderLimits, forced_pictures: ForcedPicturesView) -> Self {
        Self {
            decoder: PgsTrackDecoder::new(limits),
            store: PgsCompositionStore::new(),
            timeline_events: HashMap::new(),
            forced_pictures,
        }
    }

    /// Шкала «только forced-картинки» этой дорожки.
    pub(in crate::state::subtitles) fn forced_pictures_track(&self) -> &ActiveSubtitleTrack {
        self.forced_pictures.track()
    }

    /// Принимает пакет дорожки: декодирует и кладёт композиции в хранилище. Изменения шкалы
    /// копятся в `pending`.
    ///
    /// Разрыв потока (смена прохода чтения) здесь НЕ определяется: это решение на уровне
    /// media, его принимает [`EmbeddedPgsTracks::observe_read_run`] до приёма пакета.
    pub(in crate::state::subtitles) fn ingest_packet(
        &mut self,
        packet: &SubtitleFeedPacket,
        pending: &mut PgsPendingChanges,
        counters: &mut PgsFeedCounters,
    ) {
        let block_time = subtitle_time_from_pts(packet.presentation_time);
        let end = composition_end(packet, block_time);
        for composition in self.decoder.push_block(block_time, &packet.payload) {
            match self.store.insert(composition, end) {
                StoreChange::Duplicate => counters.duplicate_compositions += 1,
                StoreChange::AppendedAtTail(tail) => {
                    counters.compositions_added += 1;
                    pending.appended.push(tail);
                }
                StoreChange::RequiresRebuild => {
                    counters.compositions_added += 1;
                    pending.rebuild_required = true;
                }
            }
        }
    }

    /// Демуксер перепрыгнул (seek/scrub, потеря пакета): незаконченный набор декодера и открытая композиция
    /// теряются, потому что неизвестно, закончились ли они в пропущенном участке.
    fn note_read_run_changed(
        &mut self,
        pending: &mut PgsPendingChanges,
        counters: &mut PgsFeedCounters,
    ) {
        counters.discontinuities += 1;
        self.decoder.reset_after_discontinuity();
        if self.store.note_discontinuity().requires_rebuild() {
            pending.rebuild_required = true;
        }
    }

    /// Применяет политику памяти относительно `position`; `true`, если вытеснено что-то, что
    /// лежит на шкале (шкалу надо пересобрать).
    pub(in crate::state::subtitles) fn enforce_retention(
        &mut self,
        position: SubtitleTime,
        policy: PgsRetention,
        counters: &mut PgsFeedCounters,
    ) -> RetentionOutcome {
        let outcome = self.store.enforce_retention(position, policy);
        counters.evicted_compositions += outcome.evicted_compositions as u64;
        outcome
    }

    /// Сколько байт занимает хранилище (для выбора политики удержания).
    pub(in crate::state::subtitles) fn estimated_bytes(&self) -> usize {
        self.store.estimated_bytes()
    }

    /// Переносит накопленные изменения на шкалу.
    ///
    /// `force_rebuild` — пересборка нужна независимо от накопленного (вытеснение по памяти).
    /// `fresh_source` выдаёт новую идентичность источника, если шкалу придётся пересоздавать:
    /// пересборка не ломает контракт `CueKey` («тот же ключ = то же содержимое»).
    pub(in crate::state::subtitles) fn apply_to_timeline(
        &mut self,
        pending: PgsPendingChanges,
        force_rebuild: bool,
        track: &mut ActiveSubtitleTrack,
        counters: &mut PgsFeedCounters,
        mut fresh_source: impl FnMut() -> CueSourceId,
    ) -> TimelineChange {
        if pending.is_empty() && !force_rebuild {
            return TimelineChange::Untouched;
        }
        let mut must_rebuild = force_rebuild || pending.rebuild_required;
        if !must_rebuild {
            // Рассогласование (предшественник неизвестен шкале или не укорачивается) лечится
            // пересборкой из хранилища, которое — источник истины.
            must_rebuild = !self.append_to_timeline(pending.appended, track);
            if must_rebuild {
                counters.shortening_mismatches += 1;
            }
        }
        if !must_rebuild {
            return TimelineChange::Edited;
        }
        // Каждая шкала получает свою идентичность источника: у вида «только forced» события
        // имеют другие id, чем у полной шкалы.
        self.rebuild_timeline(track, fresh_source(), fresh_source());
        counters.timeline_rebuilds += 1;
        TimelineChange::Rebuilt
    }

    /// Дописывает композиции в хвост шкалы (и вида «только forced») и укорачивает их
    /// предшественников. `false` — шкала или вид не приняли изменение (рассогласование), нужна
    /// пересборка обеих.
    fn append_to_timeline(
        &mut self,
        appended: Vec<TailAppend>,
        track: &mut ActiveSubtitleTrack,
    ) -> bool {
        for tail in appended {
            if !self.forced_pictures.append(&tail) {
                return false;
            }
            if let Some(closed) = tail.predecessor_closed {
                let Some(predecessor_events) = self.timeline_events.get(&closed.composition) else {
                    return false;
                };
                for &event_id in predecessor_events {
                    if track.shorten_event_end(event_id, closed.new_end).is_err() {
                        return false;
                    }
                }
            }
            let (_report, event_ids) = track.append_events_with_ids(tail.events);
            self.timeline_events.insert(tail.composition, event_ids);
        }
        true
    }

    /// Пересоздаёт шкалу и вид «только forced» из хранилища под новыми источниками.
    fn rebuild_timeline(
        &mut self,
        track: &mut ActiveSubtitleTrack,
        source_id: CueSourceId,
        forced_source_id: CueSourceId,
    ) {
        track.restart_with_source(source_id);
        self.timeline_events.clear();
        let compositions = self.store.events_by_composition();
        self.forced_pictures
            .rebuild(forced_source_id, &compositions);
        for (handle, events) in compositions {
            let (_report, event_ids) = track.append_events_with_ids(events);
            self.timeline_events.insert(handle, event_ids);
        }
    }

    /// Общий сдвиг синхронизации media для вида «только forced».
    fn set_sync_offset(&mut self, offset: SubtitleSyncOffset) {
        self.forced_pictures.set_sync_offset(offset);
    }

    /// Статистика декодера (пропущенные/битые сегменты и т. п.).
    #[cfg(test)]
    pub(in crate::state::subtitles) fn decode_statistics(
        &self,
    ) -> subtitle_pgs::PgsDecodeStatistics {
        *self.decoder.statistics()
    }

    /// Сколько композиций сейчас в хранилище.
    #[cfg(test)]
    pub(in crate::state::subtitles) fn stored_compositions(&self) -> usize {
        self.store.len()
    }
}

/// Конец показа композиции из пакета: ненулевая длительность задаёт конец, иначе показ идёт
/// «до следующего набора».
fn composition_end(packet: &SubtitleFeedPacket, start: SubtitleTime) -> CompositionEnd {
    let Some(duration) = packet.duration.filter(|duration| !duration.is_zero()) else {
        return CompositionEnd::UntilNextDisplaySet;
    };
    match duration_micros(duration).and_then(|micros| start.as_micros().checked_add(micros)) {
        Some(end_micros) => CompositionEnd::ExplicitAt(SubtitleTime::from_micros(end_micros)),
        // Время вне модели: безопаснее считать конец неизвестным, чем выдумывать его.
        None => CompositionEnd::UntilNextDisplaySet,
    }
}

fn duration_micros(duration: Duration) -> Option<i64> {
    i64::try_from(duration.as_micros()).ok()
}

/// PGS-дорожки media. Ключ - идентификатор дорожки в списке владельца.
#[derive(Default)]
pub(in crate::state::subtitles) struct EmbeddedPgsTracks {
    tracks: HashMap<SubtitleTrackId, PgsTrackState>,
    /// Дорожка, под которую последний раз применены роли; смена означает переключение ролей.
    roles_applied_for: Option<SubtitleTrackId>,
    counters: PgsFeedCounters,
    /// Самый новый проход чтения демуксера, о котором знает media (по пакету или по снимку
    /// плеера); `None` — ещё ничего не видели. Растёт монотонно.
    latest_read_run: Option<SubtitleReadRun>,
}

/// Что решил [`EmbeddedPgsTracks::observe_read_run`] про проход чтения.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::state::subtitles) enum ReadRunAdmission {
    /// Проход актуален (или первый увиденный): пакет можно принимать.
    Accepted,
    /// Проход старше известного: пакет из трубы остался от времени до seek — отбросить.
    Stale,
}

impl EmbeddedPgsTracks {
    /// Сверяет проход чтения демуксера (из пакета или из снимка плеера) с известным media.
    ///
    /// Номера растут монотонно, поэтому сравнение порядковое, а не «равно / не равно»:
    /// - первый увиденный номер — точка отсчёта, разрыва нет;
    /// - номер больше известного — позиция чтения прыгнула (seek, scrub, потеря пакета в
    ///   трубе): РАЗРЫВ ДЛЯ ВСЕХ PGS-дорожек media сразу (декодер сброшен, открытые композиции
    ///   выброшены, изменения записаны в `batches`), даже если у других дорожек новых пакетов
    ///   ещё нет — именно так устаревший кадр не висит после seek;
    /// - номер меньше известного — [`ReadRunAdmission::Stale`], состояние не меняется.
    pub(in crate::state::subtitles) fn observe_read_run(
        &mut self,
        run: SubtitleReadRun,
        batches: &mut PgsBatches,
    ) -> ReadRunAdmission {
        match self.latest_read_run {
            Some(latest) if run < latest => return ReadRunAdmission::Stale,
            Some(latest) if run == latest => return ReadRunAdmission::Accepted,
            Some(_) => {
                for (&track_id, state) in &mut self.tracks {
                    state.note_read_run_changed(
                        batches.entry(track_id).or_default(),
                        &mut self.counters,
                    );
                }
            }
            None => {}
        }
        self.latest_read_run = Some(run);
        ReadRunAdmission::Accepted
    }

    /// То же для пакета: устаревший пакет дополнительно считается в счётчиках диагностики.
    pub(in crate::state::subtitles) fn admit_packet_read_run(
        &mut self,
        run: SubtitleReadRun,
        batches: &mut PgsBatches,
    ) -> ReadRunAdmission {
        let admission = self.observe_read_run(run, batches);
        if admission == ReadRunAdmission::Stale {
            self.counters.stale_run_packets += 1;
        }
        admission
    }

    /// Состояние дорожки; создаётся на первом обращении с лимитами декодера.
    pub(in crate::state::subtitles) fn track_or_create(
        &mut self,
        track_id: SubtitleTrackId,
        limits: PgsDecoderLimits,
        new_forced_pictures: impl FnOnce() -> ForcedPicturesView,
    ) -> (&mut PgsTrackState, &mut PgsFeedCounters) {
        let state = self
            .tracks
            .entry(track_id)
            .or_insert_with(|| PgsTrackState::new(limits, new_forced_pictures()));
        (state, &mut self.counters)
    }

    /// Шкала «только forced-картинки» дорожки; `None` - пакетов дорожки ещё не было.
    pub(in crate::state::subtitles) fn forced_pictures_view(
        &self,
        track_id: SubtitleTrackId,
    ) -> Option<&ActiveSubtitleTrack> {
        self.tracks
            .get(&track_id)
            .map(PgsTrackState::forced_pictures_track)
    }

    /// Применяет общий сдвиг синхронизации ко всем видам «только forced».
    pub(in crate::state::subtitles) fn apply_sync_offset(&mut self, offset: SubtitleSyncOffset) {
        for state in self.tracks.values_mut() {
            state.set_sync_offset(offset);
        }
    }

    /// Состояние уже известной дорожки вместе со счётчиками.
    pub(in crate::state::subtitles) fn existing_track(
        &mut self,
        track_id: SubtitleTrackId,
    ) -> Option<(&mut PgsTrackState, &mut PgsFeedCounters)> {
        let state = self.tracks.get_mut(&track_id)?;
        Some((state, &mut self.counters))
    }

    /// Забывает дорожку (она убрана из списка или пересоздана: кэш прежнего содержимого не
    /// описывает новую шкалу).
    pub(in crate::state::subtitles) fn forget_track(&mut self, track_id: SubtitleTrackId) {
        self.tracks.remove(&track_id);
        if self.roles_applied_for == Some(track_id) {
            self.roles_applied_for = None;
        }
    }

    /// Фиксирует показываемую дорожку, под которую применяются роли. `true`, если она сменилась
    /// с прошлого применения (или применялась впервые): политику памяти надо пересчитать.
    pub(in crate::state::subtitles) fn note_displayed_track(
        &mut self,
        displayed: Option<SubtitleTrackId>,
    ) -> bool {
        if self.roles_applied_for == displayed {
            return false;
        }
        self.roles_applied_for = displayed;
        true
    }

    /// Дорожки, у которых есть PGS-состояние. Дорожек единицы, а смена ролей редка, поэтому при
    /// смене пересчитываются все: не нужно помнить, какая именно была показываемой раньше.
    pub(in crate::state::subtitles) fn known_tracks(&self) -> Vec<SubtitleTrackId> {
        self.tracks.keys().copied().collect()
    }

    /// Счётчики.
    #[cfg(test)]
    pub(in crate::state::subtitles) fn counters(&self) -> PgsFeedCounters {
        self.counters
    }

    /// Состояние дорожки (только чтение, для тестов).
    #[cfg(test)]
    pub(in crate::state::subtitles) fn track(
        &self,
        track_id: SubtitleTrackId,
    ) -> Option<&PgsTrackState> {
        self.tracks.get(&track_id)
    }
}
