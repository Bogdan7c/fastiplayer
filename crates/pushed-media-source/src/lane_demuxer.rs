//! Демуксер полосы на стороне плеера: эпизоды по порядку как один поток.
//!
//! Плеер видит одну непрерывную полосу: пакеты со стабильными ID дорожек, время
//! со смещением полосы, `TracksChanged` только при настоящей смене описания и
//! `TemporarilyUnavailable`, пока страница не прислала байты. `next_event`
//! никогда не блокируется: всё ожидание — в потоках демуксеров эпизодов.

use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxSeekResult, DemuxSeekability, DemuxTrackListUpdate, Demuxer,
    MediaDemuxError, Packet, TimelineNotSeekableReason, TrackInfo, TrackTimestamp,
};

use crate::episode::EpisodeHandle;
use crate::error::PushedStreamError;
use crate::lane::{LaneId, LaneMailbox, MailboxTake};
use crate::random_access::{RandomAccessDecision, RandomAccessGate};
use crate::stream_state::StreamState;
use crate::track_identity::{TrackIdentity, TrackListChange};

/// Итог подготовки полосы к выдаче плееру.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LanePreparation {
    /// Дорожки известны.
    Ready,
    /// Первый эпизод ещё не открылся.
    Waiting,
    /// Вход полосы окончен, а медиа так и не было.
    Empty,
}

/// Что дал текущий эпизод.
#[expect(
    clippy::large_enum_variant,
    reason = "a short-lived return value that is matched immediately; boxing would allocate per packet"
)]
enum EpisodeEvent {
    /// Событие демуксера эпизода.
    Event(DemuxReadEvent),
    /// Эпизода нет, вход продолжается.
    Waiting,
    /// Эпизода нет, и новых не будет.
    InputEnded,
}

/// Демуксер одной полосы.
pub(crate) struct LaneDemuxer {
    /// Номер полосы.
    lane: LaneId,
    /// Ящик эпизодов.
    mailbox: Arc<LaneMailbox>,
    /// Общее состояние потока: длительность и первая ошибка.
    stream: Arc<StreamState>,
    /// Эпизод, который сейчас читается.
    current: Option<EpisodeHandle>,
    /// Стабильные ID дорожек.
    identity: TrackIdentity,
    /// Пакет, прочитанный при подготовке раньше `TracksChanged` (на случай,
    /// если демуксер эпизода опубликовал дорожки без события).
    held_packet: Option<Packet>,
    /// Дорожки видео, ждущие ключевой кадр после выброшенного.
    random_access: RandomAccessGate,
}

impl LaneDemuxer {
    /// Демуксер полосы поверх её ящика.
    pub(crate) fn new(lane: LaneId, mailbox: Arc<LaneMailbox>, stream: Arc<StreamState>) -> Self {
        Self {
            lane,
            mailbox,
            stream,
            current: None,
            identity: TrackIdentity::default(),
            held_packet: None,
            random_access: RandomAccessGate::default(),
        }
    }

    /// Номер полосы.
    pub(crate) const fn lane(&self) -> LaneId {
        self.lane
    }

    /// Неблокирующий шаг подготовки: открылся ли первый эпизод и известны ли дорожки.
    pub(crate) fn prepare(&mut self) -> Result<LanePreparation, PushedStreamError> {
        while !self.identity.has_published() {
            let event = match self.next_episode_event()? {
                EpisodeEvent::Event(event) => event,
                EpisodeEvent::Waiting => return Ok(LanePreparation::Waiting),
                EpisodeEvent::InputEnded => return Ok(LanePreparation::Empty),
            };
            match event {
                DemuxReadEvent::TracksChanged(update) => {
                    self.identity.adopt(&update.tracks);
                }
                DemuxReadEvent::Packet(packet) => {
                    // Демуксер опубликовал дорожки без события: берём их из `tracks()`.
                    let tracks = self
                        .current
                        .as_ref()
                        .map(|episode| episode.tracks().to_vec())
                        .unwrap_or_default();
                    if tracks.is_empty() {
                        return Err(self.stream.record_failure(PushedStreamError::Demux {
                            lane: self.lane,
                            reason: "a packet arrived before the track list".to_owned(),
                        }));
                    }
                    self.identity.adopt(&tracks);
                    self.held_packet = Some(packet);
                }
                DemuxReadEvent::EndOfStream => self.current = None,
                DemuxReadEvent::TemporarilyUnavailable(_) => return Ok(LanePreparation::Waiting),
                DemuxReadEvent::MediaMetadataChanged(_) => {}
            }
        }
        Ok(LanePreparation::Ready)
    }

    /// Следующее событие текущего эпизода (при необходимости берёт эпизод из ящика).
    fn next_episode_event(&mut self) -> Result<EpisodeEvent, PushedStreamError> {
        self.ensure_stream_healthy()?;
        if self.current.is_none() {
            match self.mailbox.take_next() {
                MailboxTake::Episode(episode) => self.current = Some(*episode),
                MailboxTake::Waiting => return Ok(EpisodeEvent::Waiting),
                MailboxTake::InputEnded => return Ok(EpisodeEvent::InputEnded),
            }
        }
        let Some(episode) = self.current.as_mut() else {
            return Ok(EpisodeEvent::Waiting);
        };
        match episode.demuxer_mut().next_event() {
            Ok(event) => Ok(EpisodeEvent::Event(event)),
            Err(error) => Err(self.record_demux_failure(&error)),
        }
    }

    /// Первая ошибка потока — ошибка и этой полосы.
    fn ensure_stream_healthy(&self) -> Result<(), PushedStreamError> {
        self.stream.failure().map_or(Ok(()), Err)
    }

    /// Запоминает ошибку демуксера как ошибку потока и возвращает её.
    fn record_demux_failure(&self, error: &anyhow::Error) -> PushedStreamError {
        self.stream.record_failure(PushedStreamError::Demux {
            lane: self.lane,
            reason: format!("{error:#}"),
        })
    }

    /// Пакет эпизода → пакет полосы: публичный ID и время со смещением.
    /// `None` — кадр до нуля после смещения, MSE его тоже выбрасывает.
    fn publish_packet(&mut self, mut packet: Packet) -> Result<Option<Packet>, PushedStreamError> {
        let episode_offset = self
            .current
            .as_ref()
            .map(EpisodeHandle::offset)
            .unwrap_or_default();
        let public_id = self.identity.public_id(packet.track_id).ok_or_else(|| {
            self.stream.record_failure(PushedStreamError::Demux {
                lane: self.lane,
                reason: format!("packet of undeclared track {}", packet.track_id),
            })
        })?;
        let Some(pts) = episode_offset.apply(packet.pts) else {
            self.random_access.note_dropped(public_id, packet.kind);
            return Ok(None);
        };
        // Исходные метки дорожки тоже сдвигаются и получают публичный номер:
        // программный декодер берёт время кадра из них и сверяет номер дорожки
        // с номером пакета.
        let shift_track = |timestamp: Option<TrackTimestamp>| {
            timestamp
                .and_then(|timestamp| episode_offset.apply_to_track_timestamp(timestamp, public_id))
        };
        // Одно решение на пакет: выброшен, если в минус ушла любая из его меток.
        if packet.track_pts.is_some() && shift_track(packet.track_pts).is_none() {
            self.random_access.note_dropped(public_id, packet.kind);
            return Ok(None);
        }
        if self
            .random_access
            .admit(public_id, packet.kind, packet.keyframe)
            == RandomAccessDecision::Drop
        {
            return Ok(None);
        }
        packet.track_pts = shift_track(packet.track_pts);
        packet.track_dts = shift_track(packet.track_dts);
        packet.track_id = public_id;
        packet.pts = pts;
        // DTS до нуля при неотрицательном PTS бывает у первых кадров с B-кадрами;
        // без DTS декодер опирается на порядок пакетов.
        packet.dts = packet.dts.and_then(|dts| episode_offset.apply(dts));
        Ok(Some(packet))
    }

    /// Публичный `TracksChanged`, если описание действительно сменилось.
    fn publish_track_change(&mut self, episode_tracks: &[TrackInfo]) -> Option<DemuxReadEvent> {
        match self.identity.adopt(episode_tracks) {
            TrackListChange::Unchanged => None,
            TrackListChange::Changed => Some(DemuxReadEvent::TracksChanged(DemuxTrackListUpdate {
                tracks: self.identity.published().to_vec(),
                duration: self.stream.duration().finite(),
            })),
        }
    }
}

impl Demuxer for LaneDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.identity.published()
    }

    fn duration(&self) -> Option<Duration> {
        self.stream.duration().finite()
    }

    fn seekability(&self) -> DemuxSeekability {
        // Перемотка — сессия 03b (порт перемотки по квитанциям).
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        }
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        if let Some(packet) = self.held_packet.take()
            && let Some(packet) = self.publish_packet(packet)?
        {
            return Ok(DemuxReadEvent::Packet(packet));
        }
        loop {
            let event = match self.next_episode_event()? {
                EpisodeEvent::Event(event) => event,
                EpisodeEvent::Waiting => {
                    return Ok(DemuxReadEvent::TemporarilyUnavailable(
                        self.stream.retry_hint(),
                    ));
                }
                EpisodeEvent::InputEnded => return Ok(DemuxReadEvent::EndOfStream),
            };
            match event {
                DemuxReadEvent::Packet(packet) => {
                    if let Some(packet) = self.publish_packet(packet)? {
                        return Ok(DemuxReadEvent::Packet(packet));
                    }
                }
                DemuxReadEvent::TracksChanged(update) => {
                    if let Some(event) = self.publish_track_change(&update.tracks) {
                        return Ok(event);
                    }
                }
                // Эпизод дочитан: следующий из ящика продолжит полосу.
                DemuxReadEvent::EndOfStream => self.current = None,
                waiting_or_metadata @ (DemuxReadEvent::TemporarilyUnavailable(_)
                | DemuxReadEvent::MediaMetadataChanged(_)) => return Ok(waiting_or_metadata),
            }
        }
    }

    fn seek(&mut self, _timestamp: Duration) -> Result<DemuxSeekResult> {
        Err(MediaDemuxError::SeekUnavailable {
            reason: "pushed media lanes do not seek yet".to_owned(),
        }
        .into())
    }
}

impl Drop for LaneDemuxer {
    /// Плеер закрыл поток: ждущие эпизоды выбрасываются, приём перестаёт их создавать.
    fn drop(&mut self) {
        self.mailbox.close_consumer();
    }
}
