//! Встроенные дорожки субтитров у владельца: список дорожек из снимка плеера и кэш реплик,
//! наполняемый потоком пакетов из трубы `player-core`.
//!
//! # Архитектура
//! - **Список дорожек.** [`SubtitleOwner::sync_embedded_tracks`] идемпотентно приводит список
//!   встроенных дорожек media к строкам снимка плеера (`kind == Subtitle`). Неизменный снимок
//!   проверяется без аллокаций и ничего не перестраивает, поэтому метод можно звать на каждом
//!   снимке. Дорожка с текстовым кодеком сразу `Ready` с пустым таймлайном (выбор мгновенный),
//!   с картинным/неизвестным — серая «not supported yet».
//! - **Кэш реплик.** Пакеты приходят для ВСЕХ дорожек (решение владельца): каждая дорожка
//!   копит собственный растущий `SubtitleTimeline`, переключение показывает уже прочитанное.
//!   Дубликаты после seek назад таймлайн отбрасывает сам.
//! - **Маршрутизация по media.** Сообщение чужого (старого) media игнорируется. Сообщение
//!   БУДУЩЕГО media (труба обгоняет приложение: плеер уже читает новое видео, а владелец ещё
//!   не получил установку) и пакет дорожки, которой нет в списке (снимок отстаёт от трубы),
//!   откладываются в ограниченный буфер и доставляются после `begin_media` / обновления списка.
//!   «Будущее» определяется порядком `MediaInstanceId`: идентификаторы выдаются по возрастанию.
//!
//! # Инварианты
//! - За один вызов читается не больше [`FEED_DRAIN_MAX_MESSAGES_PER_CALL`] сообщений: кадр UI не
//!   может зависнуть на разборе. Остаток подхватит следующий вызов (труба сама ограничена).
//! - Ошибки не молчат: пропущенные пакеты считаются в [`EmbeddedFeedCounters`], первый случай
//!   каждого вида идёт в `warn!`, остальные в `debug!`; текст реплик в логи не попадает.

use media_core::{TrackId, TrackKind};
use player_core::{
    MediaInstanceId, SubtitleFeedMessage, SubtitleFeedPacket, SubtitleFeedReceiver,
    TrackSummarySnapshot,
};
use std::collections::HashMap;
use subtitle_core::{SubtitleEvent, SubtitleTimeline, SubtitleTrackId};

use super::SubtitleOwner;
use super::active_track::ActiveSubtitleTrack;
use super::auto_select::auto_select;
use super::embedded_events::{PacketEventOutcome, PacketRejection, event_from_packet};
use super::embedded_track::EmbeddedTrackDescription;
use super::sidecar_job::SubtitleLoadFailure;
use super::track_list::TrackLoadState;

/// Сколько сообщений трубы читается за один вызов (внутренний инвариант: ограничивает работу
/// UI-потока на кадре; труба сама имеет ёмкость из конфига, остаток читается в следующий вызов).
pub(super) const FEED_DRAIN_MAX_MESSAGES_PER_CALL: usize = 2_048;

/// Сколько сообщений ждёт «своего часа» (будущее media или дорожка, которой ещё нет в списке).
/// Внутренний инвариант: буфер временный и живёт до ближайшего `begin_media` / обновления
/// списка дорожек; переполнение считается и логируется, а не растёт без предела.
pub(super) const DEFERRED_FEED_MAX_MESSAGES: usize = 4_096;

/// Изменилось ли что-то, что видит пользователь (меню или реплики показываемой дорожки).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EmbeddedUpdate {
    /// Список дорожек или реплики показываемой дорожки изменились: нужна перерисовка.
    Changed,
    /// Видимого изменения нет.
    Unchanged,
}

impl EmbeddedUpdate {
    /// Объединение: `Changed`, если изменилось хотя бы одно.
    pub(crate) fn merge(self, other: Self) -> Self {
        if self == Self::Changed || other == Self::Changed {
            Self::Changed
        } else {
            Self::Unchanged
        }
    }
}

/// Счётчики работы с трубой встроенных дорожек (диагностика и тесты).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct EmbeddedFeedCounters {
    /// Реплики, добавленные в таймлайны.
    pub(super) cues_added: u64,
    /// Дубликаты реплик (после seek назад демуксер читает те же пакеты).
    pub(super) duplicate_cues: u64,
    /// Корректные пакеты без текста (пустой промежуток).
    pub(super) empty_packets: u64,
    /// Пакеты, которые не удалось разобрать.
    pub(super) rejected_packets: u64,
    /// Реплики, не поместившиеся в `max_cues_per_file`.
    pub(super) cues_over_capacity: u64,
    /// Сообщения чужого (прежнего) media.
    pub(super) stale_messages: u64,
    /// Пакеты дорожек, которые мы не показываем (картинки, неизвестные кодеки).
    pub(super) unsupported_track_packets: u64,
    /// Сообщения, выброшенные из-за переполнения буфера ожидания.
    pub(super) deferred_overflow: u64,
}

/// Состояние чтения трубы владельцем.
#[derive(Default)]
pub(super) struct EmbeddedFeedState {
    /// Приёмник трубы; `None`, пока приложение его не передало.
    receiver: Option<SubtitleFeedReceiver>,
    /// Переиспользуемый буфер чтения (без аллокации на кадр).
    scratch: Vec<SubtitleFeedMessage>,
    /// Сообщения, ждущие media или списка дорожек.
    deferred: Vec<SubtitleFeedMessage>,
    /// Самое новое media, о котором владелец знает (`begin_media`); всё, что новее, — будущее.
    newest_known_media: Option<MediaInstanceId>,
    /// Счётчики.
    counters: EmbeddedFeedCounters,
    /// Сколько потерянных пакетов трубы уже отражено в логе.
    reported_dropped_packets: u64,
    /// Сколько потерянных «начал media» (шрифтов) уже отражено в логе.
    reported_dropped_media_starts: u64,
}

/// Куда направить сообщение трубы.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MessageRoute {
    /// Относится к текущему media владельца.
    Current,
    /// Относится к media, которое владелец ещё не получил.
    Future,
    /// Относится к прежнему media.
    Stale,
}

/// Реплики одной дорожки, накопленные за вызов (добавляются в таймлайн одним батчем).
type TrackBatches = HashMap<SubtitleTrackId, Vec<SubtitleEvent>>;

impl SubtitleOwner {
    /// Принимает приёмник трубы встроенных субтитров (приложение берёт его у `PlayerWorker`).
    pub(super) fn attach_feed_receiver(&mut self, receiver: SubtitleFeedReceiver) {
        self.embedded_feed.receiver = Some(receiver);
    }

    /// Счётчики для тестов и диагностики.
    #[cfg(test)]
    pub(super) fn embedded_feed_counters(&self) -> EmbeddedFeedCounters {
        self.embedded_feed.counters
    }

    /// Приводит список встроенных дорожек media к снимку плеера (см. документацию модуля).
    ///
    /// Снимок другого media игнорируется: список принадлежит media владельца.
    pub(super) fn sync_embedded_tracks(
        &mut self,
        snapshot_media: Option<MediaInstanceId>,
        tracks: &[TrackSummarySnapshot],
    ) -> EmbeddedUpdate {
        let Some(media) = self.media.as_ref() else {
            return EmbeddedUpdate::Unchanged;
        };
        if Some(media.media_instance_id) != snapshot_media {
            return EmbeddedUpdate::Unchanged;
        }
        let subtitle_rows = || tracks.iter().filter(|row| row.kind == TrackKind::Subtitle);
        if media.tracks.embedded_matches_snapshot(subtitle_rows()) {
            return EmbeddedUpdate::Unchanged;
        }
        self.rebuild_embedded_tracks(subtitle_rows().collect());
        EmbeddedUpdate::Changed
    }

    /// Применяет изменения набора встроенных дорожек: убирает исчезнувшие, обновляет
    /// изменившиеся и добавляет новые (в конец списка), затем повторяет автовыбор.
    fn rebuild_embedded_tracks(&mut self, rows: Vec<&TrackSummarySnapshot>) {
        let Some(media) = self.media.as_mut() else {
            return;
        };
        let wanted: Vec<TrackId> = rows.iter().map(|row| row.id).collect();
        let mut removed = media.tracks.retain_embedded(|id| wanted.contains(&id));

        for row in rows {
            let description = EmbeddedTrackDescription::from_snapshot(row);
            if let Some(existing_id) = media.tracks.find_embedded(row.id) {
                let same_codec = media
                    .tracks
                    .get(existing_id)
                    .and_then(|entry| entry.description.embedded())
                    .is_some_and(|existing| existing.has_same_codec(&description));
                if same_codec {
                    media
                        .tracks
                        .replace_embedded_description(existing_id, description);
                    continue;
                }
                // Кодек дорожки сменился: кэш реплик другого формата не годится.
                removed.extend(media.tracks.retain_embedded(|id| id != row.id));
            }
            let load = if description.is_renderable_now() {
                TrackLoadState::Ready(ActiveSubtitleTrack::new(
                    self.source_ids.allocate(),
                    SubtitleTimeline::new(),
                    media.sync_offset,
                    description.language().cloned(),
                ))
            } else {
                TrackLoadState::Unavailable(SubtitleLoadFailure::EmbeddedCodecNotSupportedYet)
            };
            media.tracks.push_embedded(description, load);
        }
        if media.selected.is_some_and(|id| removed.contains(&id)) {
            media.selected = None;
        }
        if media
            .forced_fallback
            .is_some_and(|id| removed.contains(&id))
        {
            media.forced_fallback = None;
        }
        let needs_rasterizer = media.tracks.entries().iter().any(|entry| {
            entry
                .description
                .embedded()
                .is_some_and(EmbeddedTrackDescription::is_renderable_now)
        });
        tracing::debug!(
            embedded_tracks = media.tracks.entries().len(),
            "Embedded subtitle tracks synchronized with the player"
        );

        self.reselect_automatically();
        self.presenter.forget_shown_overlay();
        self.ensure_display_track_loading();
        if needs_rasterizer {
            // Текстовым дорожкам нужен растеризатор, даже если файла рядом нет.
            self.request_rasterizer_in_background();
        }
        // Пакеты дорожек, которых ещё не было в списке, ждали именно этого момента.
        self.redeliver_deferred_feed();
    }

    /// Повторяет автовыбор по ВСЕМУ списку (файлы рядом и встроенные), пока пользователь не
    /// выбрал дорожку сам. Принудительная дорожка пересчитывается всегда.
    pub(super) fn reselect_automatically(&mut self) {
        let preferred = self.preferred_languages();
        let Some(media) = self.media.as_mut() else {
            return;
        };
        let selection = {
            let traits = media.tracks.traits();
            auto_select(&traits, &preferred)
        };
        media.forced_fallback = selection
            .forced_fallback
            .and_then(|index| media.tracks.id_at(index));
        if !media.user_chose_track {
            media.selected = selection
                .for_enabled
                .and_then(|index| media.tracks.id_at(index));
        }
    }

    /// Читает не более [`FEED_DRAIN_MAX_MESSAGES_PER_CALL`] сообщений трубы и раскладывает их
    /// по дорожкам текущего media.
    pub(super) fn drain_embedded_feed(&mut self) -> EmbeddedUpdate {
        let Some(receiver) = self.embedded_feed.receiver.as_ref() else {
            return EmbeddedUpdate::Unchanged;
        };
        let mut messages = std::mem::take(&mut self.embedded_feed.scratch);
        messages.clear();
        receiver.drain_into(&mut messages, FEED_DRAIN_MAX_MESSAGES_PER_CALL);
        self.report_feed_losses();

        let update = self.route_messages(messages.drain(..));
        // Буфер возвращается владельцу: следующий вызов не аллоцирует.
        self.embedded_feed.scratch = messages;
        update
    }

    /// Сообщает о потерях трубы: первая потеря — предупреждение, дальше — отладка.
    fn report_feed_losses(&mut self) {
        let Some(receiver) = self.embedded_feed.receiver.as_ref() else {
            return;
        };
        let (dropped_packets, dropped_media_starts) =
            (receiver.dropped_packets(), receiver.dropped_media_starts());
        let state = &mut self.embedded_feed;
        if dropped_packets > state.reported_dropped_packets {
            if state.reported_dropped_packets == 0 {
                tracing::warn!(
                    dropped_packets,
                    "Embedded subtitle packets were dropped because the queue was full; some cues may be missing"
                );
            } else {
                tracing::debug!(
                    dropped_packets,
                    "More embedded subtitle packets were dropped"
                );
            }
            state.reported_dropped_packets = dropped_packets;
        }
        if dropped_media_starts > state.reported_dropped_media_starts {
            tracing::warn!(
                dropped_media_starts,
                "A media start message was dropped; attached fonts of that media are lost"
            );
            state.reported_dropped_media_starts = dropped_media_starts;
        }
    }

    /// Доставляет отложенные сообщения заново (после `begin_media` или обновления списка).
    pub(super) fn redeliver_deferred_feed(&mut self) -> EmbeddedUpdate {
        if self.embedded_feed.deferred.is_empty() {
            return EmbeddedUpdate::Unchanged;
        }
        let parked = std::mem::take(&mut self.embedded_feed.deferred);
        self.route_messages(parked)
    }

    /// Раскладывает сообщения: события копятся по дорожкам и добавляются одним батчем.
    fn route_messages(
        &mut self,
        messages: impl IntoIterator<Item = SubtitleFeedMessage>,
    ) -> EmbeddedUpdate {
        let mut batches = TrackBatches::new();
        for message in messages {
            self.route_message(message, &mut batches);
        }
        self.apply_batches(batches)
    }

    fn route_message(&mut self, message: SubtitleFeedMessage, batches: &mut TrackBatches) {
        let media_instance_id = match &message {
            SubtitleFeedMessage::MediaStarted(start) => start.media_instance_id,
            SubtitleFeedMessage::Packet(packet) => packet.media_instance_id,
        };
        match self.route_of(media_instance_id) {
            MessageRoute::Stale => {
                self.embedded_feed.counters.stale_messages += 1;
            }
            MessageRoute::Future => self.defer_message(message),
            MessageRoute::Current => match message {
                SubtitleFeedMessage::MediaStarted(start) => {
                    self.start_attached_fonts(start.media_instance_id, &start.attachments);
                }
                SubtitleFeedMessage::Packet(packet) => self.accept_packet(packet, batches),
            },
        }
    }

    /// Текущее, будущее или прежнее media.
    fn route_of(&self, media_instance_id: MediaInstanceId) -> MessageRoute {
        if self
            .media
            .as_ref()
            .is_some_and(|media| media.media_instance_id == media_instance_id)
        {
            return MessageRoute::Current;
        }
        match self.embedded_feed.newest_known_media {
            Some(newest) if media_instance_id <= newest => MessageRoute::Stale,
            Some(_) | None => MessageRoute::Future,
        }
    }

    /// Откладывает сообщение; при переполнении буфера выбрасывает с учётом в счётчике.
    fn defer_message(&mut self, message: SubtitleFeedMessage) {
        let state = &mut self.embedded_feed;
        if state.deferred.len() >= DEFERRED_FEED_MAX_MESSAGES {
            state.counters.deferred_overflow += 1;
            if state.counters.deferred_overflow == 1 {
                tracing::warn!(
                    "Too many embedded subtitle messages are waiting; dropping new ones"
                );
            }
            return;
        }
        state.deferred.push(message);
    }

    /// Пакет текущего media: разбирает и ставит реплику в батч своей дорожки.
    fn accept_packet(&mut self, packet: SubtitleFeedPacket, batches: &mut TrackBatches) {
        let embedded_limits = self.settings.embedded_limits;
        let Some(media) = self.media.as_ref() else {
            return;
        };
        let Some(track_id) = media.tracks.find_embedded(packet.track_id) else {
            // Снимок плеера отстал от трубы: дорожка появится в списке чуть позже.
            self.defer_message(SubtitleFeedMessage::Packet(packet));
            return;
        };
        let cue_format = media
            .tracks
            .get(track_id)
            .and_then(|entry| entry.description.embedded())
            .and_then(EmbeddedTrackDescription::cue_format);
        let Some(cue_format) = cue_format else {
            self.embedded_feed.counters.unsupported_track_packets += 1;
            return;
        };
        let outcome = event_from_packet(
            &packet,
            cue_format,
            &embedded_limits.cue_parse,
            embedded_limits.cue_max_open_duration,
        );
        match outcome {
            PacketEventOutcome::Event(event) => batches.entry(track_id).or_default().push(event),
            PacketEventOutcome::Empty => self.embedded_feed.counters.empty_packets += 1,
            PacketEventOutcome::Rejected(rejection) => self.note_rejected_packet(rejection),
        }
    }

    /// Считает отказ и пишет его в лог без текста реплики: первый — `warn!`, остальные — `debug!`.
    fn note_rejected_packet(&mut self, rejection: PacketRejection) {
        let counters = &mut self.embedded_feed.counters;
        counters.rejected_packets += 1;
        if counters.rejected_packets == 1 {
            tracing::warn!(?rejection, "An embedded subtitle packet was skipped");
        } else {
            tracing::debug!(?rejection, "An embedded subtitle packet was skipped");
        }
    }

    /// Добавляет накопленные реплики в таймлайны дорожек одним батчем на дорожку.
    fn apply_batches(&mut self, batches: TrackBatches) -> EmbeddedUpdate {
        let max_cues = self.settings.embedded_limits.max_cues_per_track;
        let Some(media) = self.media.as_mut() else {
            return EmbeddedUpdate::Unchanged;
        };
        let displayed = media.display_track_id();
        let mut update = EmbeddedUpdate::Unchanged;
        for (track_id, mut events) in batches {
            let Some(TrackLoadState::Ready(track)) =
                media.tracks.get_mut(track_id).map(|entry| &mut entry.load)
            else {
                continue;
            };
            let room = max_cues.saturating_sub(track.timeline().len());
            if events.len() > room {
                let excess = events.len() - room;
                events.truncate(room);
                let counters = &mut self.embedded_feed.counters;
                let first_overflow = counters.cues_over_capacity == 0;
                counters.cues_over_capacity += u64::try_from(excess).unwrap_or(u64::MAX);
                if first_overflow {
                    tracing::warn!(
                        limit = max_cues,
                        "An embedded subtitle track reached the cue limit; new cues are skipped"
                    );
                }
            }
            let report = track.append_events(events);
            let counters = &mut self.embedded_feed.counters;
            counters.cues_added += report.inserted as u64;
            counters.duplicate_cues += report.duplicates_ignored as u64;
            counters.cues_over_capacity += report.rejected_over_capacity as u64;
            if report.inserted > 0 && Some(track_id) == displayed {
                update = EmbeddedUpdate::Changed;
            }
        }
        update
    }

    /// Новое media установлено: запоминает его как самое новое известное и доставляет
    /// сообщения, которые труба успела принести раньше установки.
    pub(super) fn adopt_media_for_embedded_feed(&mut self, media_instance_id: MediaInstanceId) {
        let newest = &mut self.embedded_feed.newest_known_media;
        if newest.is_none_or(|known| media_instance_id > known) {
            *newest = Some(media_instance_id);
        }
        self.redeliver_deferred_feed();
    }

    /// Для тестов: доставляет сообщения так, как если бы их принёс приёмник трубы (настоящий
    /// приёмник создаёт только `player-core`; его собственная работа проверена там).
    #[cfg(test)]
    pub(super) fn deliver_feed_messages(
        &mut self,
        messages: Vec<SubtitleFeedMessage>,
    ) -> EmbeddedUpdate {
        self.route_messages(messages)
    }
}
