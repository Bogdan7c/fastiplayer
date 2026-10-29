//! Поток — одно поколение «сегментов извне» (у браузера — один `MediaSource`).
//!
//! `pushed_stream` создаёт две стороны:
//! - [`PushedStreamWriter`] — у того, кто приносит байты: полосы, куски, `abort`,
//!   `changeType`, смещение времени, длительность, конец потока;
//! - [`PushedStreamOpener`] — у того, кто готовит воспроизведение: ждёт, пока у
//!   каждой полосы откроется первый эпизод, и отдаёт плееру один [`PushedStreamDemuxer`].
//!
//! Новое поколение — новый поток целиком: старый просто уничтожается.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use anyhow::Result;
use demux_api::{CompositeAvDemuxer, CompositeAvTrackSelection, DemuxRegistry};
use media_core::{
    DemuxReadEvent, DemuxRetryHint, DemuxSeekResult, DemuxSeekability, Demuxer, MediaDemuxError,
    TimelineNotSeekableReason, TrackInfo, TrackKind,
};
use mpeg_ts_demux::{MpegTsDemuxFactory, MpegTsDemuxOptions};
use symphonia_demux::{DemuxerOptions, SymphoniaDemuxFactory};

use crate::container::PushedContainer;
use crate::episode::EpisodeFactory;
use crate::error::{LaneLayoutProblem, PushedStreamError};
use crate::intake::IntakeLedger;
use crate::lane::{AppendOutcome, LaneId, LaneIntake, LaneMailbox};
use crate::lane_demuxer::{LaneDemuxer, LanePreparation};
use crate::policy::PushedSourcePolicy;
use crate::stream_state::StreamState;
use crate::time::{PushedDuration, TimestampOffset};

/// Не удалось собрать реестр демуксеров потока.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the pushed media demux registry could not be built: {reason}")]
pub struct PushedStreamStartError {
    /// Причина.
    reason: String,
}

/// Создаёт поток: сторону приёма и сторону открытия.
pub fn pushed_stream(
    policy: PushedSourcePolicy,
) -> Result<(PushedStreamWriter, PushedStreamOpener), PushedStreamStartError> {
    let registry = Arc::new(pushed_demux_registry()?);
    let state = Arc::new(StreamState::new(policy.retry_hint()));
    let roster = Arc::new(LaneRoster::default());
    let ledger = Arc::new(IntakeLedger::new(policy.max_held_stream_bytes()));
    let writer = PushedStreamWriter {
        state: Arc::clone(&state),
        roster: Arc::clone(&roster),
        episodes: EpisodeFactory::new(registry, ledger, policy),
        policy,
        lanes: BTreeMap::new(),
        input_ended: false,
    };
    let opener = PushedStreamOpener {
        state,
        roster,
        lanes: Vec::new(),
        policy,
        opened: false,
    };
    Ok((writer, opener))
}

/// Демуксеры, которые умеют контейнеры источника: fMP4 и WebM — `symphonia-demux`,
/// MPEG-TS — `mpeg-ts-demux`. Тот же набор, что у живого HLS.
pub(crate) fn pushed_demux_registry() -> Result<DemuxRegistry, PushedStreamStartError> {
    let start_error = |reason: String| PushedStreamStartError { reason };
    let symphonia = SymphoniaDemuxFactory::new(DemuxerOptions::default())
        .map_err(|error| start_error(error.to_string()))?;
    let mpeg_ts = MpegTsDemuxFactory::new(MpegTsDemuxOptions::default())
        .map_err(|error| start_error(error.to_string()))?;
    let mut registry = DemuxRegistry::new();
    registry
        .register(Box::new(symphonia))
        .map_err(|error| start_error(error.to_string()))?;
    registry
        .register(Box::new(mpeg_ts))
        .map_err(|error| start_error(error.to_string()))?;
    Ok(registry)
}

/// Список объявленных полос, общий для обеих сторон.
#[derive(Debug, Default)]
struct LaneRoster {
    /// Полосы и их ящики; замораживается, когда поток отдан плееру.
    state: Mutex<RosterState>,
}

/// Содержимое списка полос.
#[derive(Debug, Default)]
struct RosterState {
    /// Полосы в порядке объявления.
    declared: Vec<(LaneId, Arc<LaneMailbox>)>,
    /// Поток отдан плееру: новые полосы не принимаются.
    frozen: bool,
}

impl LaneRoster {
    /// Захватывает список; отравленный mutex не мешает работе.
    fn lock(&self) -> MutexGuard<'_, RosterState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Итог `end_of_stream`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndOfStreamReport {
    /// Байты недособранных сегментов, выброшенные в конце (у честной страницы — 0).
    pub dropped_incomplete_bytes: usize,
}

/// Сторона приёма.
#[derive(Debug)]
pub struct PushedStreamWriter {
    /// Общее состояние потока.
    state: Arc<StreamState>,
    /// Объявленные полосы.
    roster: Arc<LaneRoster>,
    /// Запуск эпизодов.
    episodes: EpisodeFactory,
    /// Пределы.
    policy: PushedSourcePolicy,
    /// Приёмные стороны полос.
    lanes: BTreeMap<LaneId, LaneIntake>,
    /// Был ли конец потока.
    input_ended: bool,
}

impl PushedStreamWriter {
    /// Объявляет полосу с контейнером (`addSourceBuffer`).
    pub fn add_lane(
        &mut self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError> {
        self.guarded(|writer| {
            if writer.lanes.contains_key(&lane) {
                return Err(PushedStreamError::LaneAlreadyDeclared { lane });
            }
            if writer.lanes.len() >= writer.policy.max_lanes() {
                return Err(PushedStreamError::TooManyLanes {
                    limit: writer.policy.max_lanes(),
                });
            }
            let mailbox = Arc::new(LaneMailbox::default());
            {
                let mut roster = writer.roster.lock();
                if roster.frozen {
                    return Err(PushedStreamError::LayoutFrozen { lane });
                }
                roster.declared.push((lane, Arc::clone(&mailbox)));
            }
            let intake = LaneIntake::new(
                lane,
                container,
                writer.policy.framing_limits(),
                writer.policy.max_pending_episodes_per_lane(),
                mailbox,
            );
            writer.lanes.insert(lane, intake);
            Ok(())
        })
    }

    /// Принимает кусок байтов полосы (`appendBuffer`).
    pub fn append(
        &mut self,
        lane: LaneId,
        chunk: &[u8],
    ) -> Result<AppendOutcome, PushedStreamError> {
        self.guarded(|writer| {
            writer.ensure_input_open()?;
            let episodes = writer.episodes.clone();
            writer.lane_mut(lane)?.append(chunk, &episodes)
        })
    }

    /// Выбрасывает недособранный сегмент полосы (`abort`).
    pub fn abort(&mut self, lane: LaneId) -> Result<(), PushedStreamError> {
        self.guarded(|writer| {
            writer.lane_mut(lane)?.abort();
            Ok(())
        })
    }

    /// Меняет контейнер или кодек полосы (`changeType`).
    pub fn change_container(
        &mut self,
        lane: LaneId,
        container: PushedContainer,
    ) -> Result<(), PushedStreamError> {
        self.guarded(|writer| {
            writer.lane_mut(lane)?.change_container(container);
            Ok(())
        })
    }

    /// Новое смещение времени полосы (`timestampOffset`).
    pub fn set_timestamp_offset(
        &mut self,
        lane: LaneId,
        offset: TimestampOffset,
    ) -> Result<(), PushedStreamError> {
        self.guarded(|writer| writer.lane_mut(lane)?.set_timestamp_offset(offset))
    }

    /// Новая длительность потока (`duration`).
    pub fn set_duration(&self, duration: PushedDuration) {
        self.state.set_duration(duration);
    }

    /// Конец потока (`endOfStream`): демуксеры дочитывают принятое, плеер получит EOF.
    pub fn end_of_stream(&mut self) -> Result<EndOfStreamReport, PushedStreamError> {
        self.guarded(|writer| {
            writer.input_ended = true;
            let episodes = writer.episodes.clone();
            let mut dropped_incomplete_bytes = 0;
            for intake in writer.lanes.values_mut() {
                dropped_incomplete_bytes += intake.end_input(&episodes)?;
            }
            if dropped_incomplete_bytes > 0 {
                tracing::warn!(
                    dropped_incomplete_bytes,
                    "Pushed media stream ended with incomplete segments; they were dropped"
                );
            }
            Ok(EndOfStreamReport {
                dropped_incomplete_bytes,
            })
        })
    }

    /// Первая ошибка потока, если он мёртв.
    #[must_use]
    pub fn failure(&self) -> Option<PushedStreamError> {
        self.state.failure()
    }

    /// Сколько принятых байтов поток держит в памяти: недособранные сегменты
    /// сканеров плюс целые сегменты, которые демуксеры ещё не прочитали.
    #[must_use]
    pub fn held_bytes(&self) -> usize {
        let partial_segments: usize = self
            .lanes
            .values()
            .map(LaneIntake::partial_segment_bytes)
            .sum();
        partial_segments + self.episodes.ledger().held_bytes()
    }

    /// Мёртвый поток отвечает первой ошибкой; новая ошибка запоминается как первая.
    fn guarded<T>(
        &mut self,
        operation: impl FnOnce(&mut Self) -> Result<T, PushedStreamError>,
    ) -> Result<T, PushedStreamError> {
        if let Some(failure) = self.state.failure() {
            return Err(failure);
        }
        operation(self).map_err(|error| self.state.record_failure(error))
    }

    /// После конца потока байты не принимаются.
    fn ensure_input_open(&self) -> Result<(), PushedStreamError> {
        if self.input_ended {
            Err(PushedStreamError::InputAfterEnd)
        } else {
            Ok(())
        }
    }

    /// Приёмная сторона объявленной полосы.
    fn lane_mut(&mut self, lane: LaneId) -> Result<&mut LaneIntake, PushedStreamError> {
        self.lanes
            .get_mut(&lane)
            .ok_or(PushedStreamError::UnknownLane { lane })
    }
}

impl Drop for PushedStreamWriter {
    /// Источник байтов ушёл без `endOfStream` (оборвался канал, закрылась вкладка):
    /// плеер доигрывает принятое и получает обычный конец потока, а не вечное
    /// ожидание. У мёртвого потока плеер и так получит его первую ошибку.
    fn drop(&mut self) {
        if self.input_ended || self.state.failure().is_some() {
            return;
        }
        if let Err(error) = self.end_of_stream() {
            tracing::warn!(%error, "Pushed media stream could not be ended after its source left");
        }
    }
}

/// Ход открытия потока.
pub enum OpenProgress {
    /// Первые эпизоды ещё не открылись: спросить снова через подсказку.
    Waiting(DemuxRetryHint),
    /// Демуксер для плеера готов, дорожки известны.
    Ready(PushedStreamDemuxer),
}

/// Сторона открытия.
pub struct PushedStreamOpener {
    /// Общее состояние потока.
    state: Arc<StreamState>,
    /// Объявленные полосы.
    roster: Arc<LaneRoster>,
    /// Демуксеры полос, известных на прошлом шаге.
    lanes: Vec<LaneDemuxer>,
    /// Пределы.
    policy: PushedSourcePolicy,
    /// Демуксер уже выдан плееру.
    opened: bool,
}

impl PushedStreamOpener {
    /// Неблокирующий шаг: готов ли поток к выдаче плееру.
    ///
    /// Готов, когда у каждой объявленной полосы открылся первый эпизод (или её
    /// вход кончился пустым). Тогда список полос замораживается, а полосы
    /// собираются в один демуксер: одна полоса — как есть, видео + звук — через
    /// существующий `CompositeAvDemuxer`.
    pub fn poll_open(&mut self) -> Result<OpenProgress, PushedStreamError> {
        if let Some(failure) = self.state.failure() {
            return Err(failure);
        }
        // Повторное открытие завело бы вторые демуксеры на тех же ящиках, и они
        // делили бы эпизоды с первым. Это ошибка вызывающего, поток не портится.
        if self.opened {
            return Err(PushedStreamError::AlreadyOpened);
        }
        self.adopt_declared_lanes();
        let waiting = OpenProgress::Waiting(self.state.retry_hint());
        if self.lanes.is_empty() {
            return Ok(waiting);
        }
        let mut preparations = Vec::with_capacity(self.lanes.len());
        for lane in &mut self.lanes {
            preparations.push(lane.prepare()?);
        }
        if preparations.contains(&LanePreparation::Waiting) || !self.freeze_roster() {
            return Ok(waiting);
        }
        let media_lanes: Vec<LaneDemuxer> = std::mem::take(&mut self.lanes)
            .into_iter()
            .zip(preparations)
            .filter(|(_, preparation)| *preparation == LanePreparation::Ready)
            .map(|(lane, _)| lane)
            .collect();
        let demuxer = self
            .assemble(media_lanes)
            .map_err(|error| self.state.record_failure(error))?;
        self.opened = true;
        Ok(OpenProgress::Ready(PushedStreamDemuxer {
            inner: demuxer,
            state: Arc::clone(&self.state),
        }))
    }

    /// Заводит демуксеры для полос, объявленных с прошлого шага.
    fn adopt_declared_lanes(&mut self) {
        let roster = self.roster.lock();
        for (lane, mailbox) in roster.declared.iter().skip(self.lanes.len()) {
            self.lanes.push(LaneDemuxer::new(
                *lane,
                Arc::clone(mailbox),
                Arc::clone(&self.state),
            ));
        }
    }

    /// Замораживает список, если за время подготовки не объявили новых полос.
    fn freeze_roster(&self) -> bool {
        let mut roster = self.roster.lock();
        if roster.declared.len() != self.lanes.len() {
            return false;
        }
        roster.frozen = true;
        true
    }

    /// Собирает полосы с медиа в один демуксер.
    fn assemble(
        &self,
        mut media_lanes: Vec<LaneDemuxer>,
    ) -> Result<Box<dyn Demuxer + Send>, PushedStreamError> {
        match media_lanes.len() {
            0 => Err(PushedStreamError::UnsupportedLayout {
                problem: LaneLayoutProblem::NoMedia,
            }),
            1 => Ok(Box::new(media_lanes.remove(0))),
            2 => self.assemble_video_and_audio(media_lanes),
            count => Err(PushedStreamError::UnsupportedLayout {
                problem: LaneLayoutProblem::TooManyMediaLanes { count },
            }),
        }
    }

    /// Видео из одной полосы, звук из другой — как раздельные дорожки DASH.
    fn assemble_video_and_audio(
        &self,
        media_lanes: Vec<LaneDemuxer>,
    ) -> Result<Box<dyn Demuxer + Send>, PushedStreamError> {
        let not_video_and_audio = PushedStreamError::UnsupportedLayout {
            problem: LaneLayoutProblem::NotOneVideoAndOneAudio,
        };
        let (mut video_lanes, mut audio_lanes): (Vec<_>, Vec<_>) = media_lanes
            .into_iter()
            .partition(|lane| first_track(lane.tracks(), TrackKind::Video).is_some());
        if video_lanes.len() != 1 || audio_lanes.len() != 1 {
            return Err(not_video_and_audio);
        }
        let (video, audio) = (video_lanes.remove(0), audio_lanes.remove(0));
        let (Some(video_track), Some(audio_track)) = (
            first_track(video.tracks(), TrackKind::Video),
            first_track(audio.tracks(), TrackKind::Audio),
        ) else {
            return Err(not_video_and_audio);
        };
        let audio_lane = audio.lane();
        let selection = CompositeAvTrackSelection::new(video_track, audio_track);
        let composite = CompositeAvDemuxer::new(
            Box::new(video),
            Box::new(audio),
            selection,
            self.policy.composite_lead_policy(),
        )
        .map_err(|error| PushedStreamError::Demux {
            lane: audio_lane,
            reason: error.to_string(),
        })?;
        Ok(Box::new(composite))
    }
}

/// ID первой дорожки вида.
fn first_track(tracks: &[TrackInfo], kind: TrackKind) -> Option<media_core::TrackId> {
    tracks
        .iter()
        .find(|track| track.kind == kind)
        .map(|track| track.id)
}

/// Демуксер потока для плеера.
pub struct PushedStreamDemuxer {
    /// Одна полоса или сборка «видео + звук».
    inner: Box<dyn Demuxer + Send>,
    /// Общее состояние потока.
    state: Arc<StreamState>,
}

impl Demuxer for PushedStreamDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        self.inner.tracks()
    }

    fn duration(&self) -> Option<Duration> {
        self.state.duration().finite()
    }

    fn seekability(&self) -> DemuxSeekability {
        // Перемотка — сессия 03b (порт перемотки по квитанциям).
        DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        }
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        if let Some(failure) = self.state.failure() {
            return Err(failure.into());
        }
        self.inner.next_event()
    }

    fn seek(&mut self, _timestamp: Duration) -> Result<DemuxSeekResult> {
        Err(MediaDemuxError::SeekUnavailable {
            reason: "pushed media streams do not seek yet".to_owned(),
        }
        .into())
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
