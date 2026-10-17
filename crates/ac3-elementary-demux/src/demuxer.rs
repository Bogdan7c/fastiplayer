//! Demuxer сырого AC-3 / E-AC-3: одна аудиодорожка, пакеты = access unit-ы.

use std::num::NonZeroU64;
use std::time::Duration;

use ac3_syncframe::{AccessUnit, SyncframeCodec};
use anyhow::Result;
use bytes::Bytes;
use demux_api::DemuxInput;
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability, Demuxer,
    MediaTime, Packet, PacketDecodeStartInitialization, PacketKeyframe, TimeBase,
    TimelineNotSeekableReason, TrackDuration, TrackId, TrackInfo, TrackKind, TrackLabels,
    TrackTimestamp,
};
use source_core::CancellationToken;
use tracing::warn;

use crate::framing::{FramingStep, SkipReason, SyncState, next_framing_step};
use crate::leading_tags::{ID3V2_HEADER_BYTES, LeadingTag, MAX_LEADING_TAGS, classify_leading_tag};
use crate::reader::ElementaryByteReader;
use crate::timeline::{ByteRateEstimate, StreamTimeline};
use crate::{Ac3ElementaryDemuxError, Ac3ElementaryDemuxOptions};

/// Codec id, общие с MKV/MP4 (`symphonia-demux`) и MPEG-TS путями.
const AC3_CODEC_ID: &str = "A_AC3";
const ENHANCED_AC3_CODEC_ID: &str = "A_EAC3";

/// У сырого потока одна дорожка.
const AUDIO_TRACK_ID: TrackId = TrackId::new(1);

/// Сколько раз seek отступает на кадр назад, если оценка промахнулась вперёд.
const MAX_SEEK_BACKOFF_STEPS: u64 = 4;

/// Первый access unit окна открытия и измерения по окну.
struct InitialScan {
    first_unit: AccessUnit,
    /// Смещение первого пакета от начала непотреблённого буфера.
    first_unit_buffer_offset: usize,
    estimate: ByteRateEstimate,
}

/// Состояние чтения, которое seek обязан вернуть при неудаче.
#[derive(Clone, Copy)]
struct ReadPositionSnapshot {
    pending_start_offset: u64,
    sync_state: SyncState,
    next_pts_samples: u64,
    reached_end: bool,
}

/// Demuxer сырого AC-3 / E-AC-3 потока.
pub struct Ac3ElementaryDemuxer {
    reader: ElementaryByteReader,
    options: Ac3ElementaryDemuxOptions,
    tracks: Vec<TrackInfo>,
    timeline: StreamTimeline,
    sync_state: SyncState,
    /// PTS следующего пакета в единицах временной базы дорожки.
    next_pts_samples: u64,
    /// Сколько байт подряд отброшено без декодируемого пакета (предел resync-а).
    discarded_without_unit: usize,
    /// Абсолютное смещение, с которого начались отбрасываемые байты (для `SyncLost`).
    discard_started_at: u64,
    reached_end: bool,
}

impl Ac3ElementaryDemuxer {
    /// Открывает поток: пропускает ID3v2, находит первый пакет и меряет окно открытия.
    pub fn open(
        input: DemuxInput,
        cancellation: CancellationToken,
        options: Ac3ElementaryDemuxOptions,
    ) -> Result<Self, Ac3ElementaryDemuxError> {
        let mut reader =
            ElementaryByteReader::open(input, cancellation, options.read_chunk_bytes.get())?;
        skip_leading_tags(&mut reader, options)?;
        reader.fill_to(options.initial_scan_bytes.get())?;
        let scan = scan_initial_window(&reader)?;
        // Мусор перед первым пакетом выбрасывается сразу: дальше поток стартует
        // с проверенной границы, а время отсчитывается от этого пакета.
        reader.consume(scan.first_unit_buffer_offset);

        let header = scan.first_unit.primary_header();
        let sample_rate = header.sample_rate_hz();
        let time_base = TimeBase::new(1, sample_rate)
            .ok_or(Ac3ElementaryDemuxError::UnrepresentableTimeBase { sample_rate })?;
        let rate = NonZeroU64::new(u64::from(sample_rate))
            .ok_or(Ac3ElementaryDemuxError::UnrepresentableTimeBase { sample_rate })?;
        let timeline = StreamTimeline::new(
            time_base,
            rate,
            reader.pending_start_offset(),
            reader.content_length(),
            scan.estimate,
        );
        let tracks = vec![track_info(&scan.first_unit, &timeline)];
        Ok(Self {
            reader,
            options,
            tracks,
            timeline,
            sync_state: SyncState::Searching,
            next_pts_samples: 0,
            discarded_without_unit: 0,
            discard_started_at: 0,
            reached_end: false,
        })
    }

    /// Следующий пакет, EOF или ошибка; мусор и оборванный хвост пропускаются с warn.
    fn read_next_event(&mut self) -> Result<DemuxReadEvent, Ac3ElementaryDemuxError> {
        loop {
            if self.reached_end {
                return Ok(DemuxReadEvent::EndOfStream);
            }
            let step = next_framing_step(
                self.reader.pending(),
                self.sync_state,
                self.reader.continuation(),
            );
            let resync_start_offset = self.reader.pending_start_offset();
            self.sync_state = step.sync_state_after(self.sync_state);
            match step {
                FramingStep::AccessUnit {
                    skipped_bytes,
                    unit,
                } => {
                    self.note_discarded(skipped_bytes, resync_start_offset)?;
                    self.reader.consume(skipped_bytes);
                    // Открытие и seek ставят буфер ровно на пакет, поэтому
                    // ненулевой счётчик здесь — только дыра посреди потока.
                    let lost_samples = self
                        .timeline
                        .samples_for_lost_bytes(self.discarded_without_unit as u64);
                    self.next_pts_samples = self.next_pts_samples.saturating_add(lost_samples);
                    let packet = self.packet_from_unit(&unit);
                    self.reader.consume(unit.total_length_bytes());
                    self.discarded_without_unit = 0;
                    return Ok(DemuxReadEvent::Packet(packet));
                }
                FramingStep::Skip { bytes, reason } => {
                    if reason == SkipReason::LostSync && self.discarded_without_unit == 0 {
                        warn!(
                            offset = resync_start_offset,
                            "AC-3 elementary stream lost sync; searching for the next syncframe"
                        );
                    }
                    self.note_discarded(bytes, resync_start_offset)?;
                    self.reader.consume(bytes);
                }
                FramingStep::NeedMoreData { discardable_bytes } => {
                    self.note_discarded(discardable_bytes, resync_start_offset)?;
                    self.reader.consume(discardable_bytes);
                    if self.reader.is_input_exhausted() {
                        // Вход кончился, а остаток не складывается в кадр
                        // (обрывок заголовка или мусор в конце файла).
                        self.finish_with_trailing_bytes();
                    } else {
                        self.reader.read_more()?;
                    }
                }
                FramingStep::TruncatedTail { offset } => {
                    self.reader.consume(offset);
                    self.finish_with_trailing_bytes();
                }
            }
        }
    }

    /// Конец потока: все целые пакеты уже отданы, обрывок последнего кадра
    /// декодировать нельзя. Это нормальный конец обрезанного файла, а не ошибка,
    /// но и не молчание — остаток попадает в лог.
    fn finish_with_trailing_bytes(&mut self) {
        let trailing_bytes = self.reader.pending().len();
        if trailing_bytes > 0 {
            warn!(
                offset = self.reader.pending_start_offset(),
                trailing_bytes,
                "AC-3 elementary stream ends with a truncated syncframe; the tail is not decodable"
            );
        }
        self.reader.consume(trailing_bytes);
        self.reached_end = true;
    }

    /// Учитывает отброшенные байты и обрывает бесконечный поиск синхронизации.
    fn note_discarded(
        &mut self,
        byte_count: usize,
        current_offset: u64,
    ) -> Result<(), Ac3ElementaryDemuxError> {
        if self.discarded_without_unit == 0 && byte_count > 0 {
            self.discard_started_at = current_offset;
        }
        self.discarded_without_unit = self.discarded_without_unit.saturating_add(byte_count);
        if self.discarded_without_unit > self.options.max_resync_bytes.get() {
            return Err(Ac3ElementaryDemuxError::SyncLost {
                resync_start_offset: self.discard_started_at,
                discarded_bytes: self.discarded_without_unit,
            });
        }
        Ok(())
    }

    /// Пакет из access unit-а в начале буфера; двигает часы на длительность пакета.
    fn packet_from_unit(&mut self, unit: &AccessUnit) -> Packet {
        let header = unit.primary_header();
        let duration_units = self
            .timeline
            .unit_samples_in_track_units(unit.samples_per_frame(), header.sample_rate_hz());
        let pts_units = self.next_pts_samples;
        self.next_pts_samples = pts_units.saturating_add(duration_units);
        let time_base = self.timeline.time_base();
        // В декодер идёт только основная программа: программы 1–7 E-AC-3 —
        // альтернативный звук, декодер перезаписал бы им PCM основной.
        let primary_program = &self.reader.pending()[..unit.primary_program_length_bytes()];
        Packet::new_with_keyframe_unbounded(
            AUDIO_TRACK_ID,
            TrackKind::Audio,
            time_base.timestamp_to_duration(pts_units),
            None,
            // Как у остальных demuxer-ов: аудио кадры не помечаются keyframe-ами.
            PacketKeyframe::NotKeyframe,
            Bytes::copy_from_slice(primary_program),
        )
        .with_track_timestamps(
            Some(TrackTimestamp::from_unsigned_units(
                AUDIO_TRACK_ID,
                pts_units,
                time_base,
            )),
            None,
        )
        .with_duration(time_base.timestamp_to_duration(duration_units))
        .with_track_duration(TrackDuration::new(
            AUDIO_TRACK_ID,
            duration_units,
            time_base,
        ))
        .with_byte_offset(self.reader.pending_start_offset())
        .with_decode_start_initialization(
            PacketDecodeStartInitialization::RequiresTrackConfiguration,
        )
    }

    /// Seek с откатом: при любой ошибке чтение остаётся там, где было.
    fn seek_transactional(
        &mut self,
        target: Duration,
    ) -> Result<DemuxSeekResult, Ac3ElementaryDemuxError> {
        if !self.reader.is_seekable() {
            return Err(Ac3ElementaryDemuxError::NotSeekable);
        }
        let snapshot = ReadPositionSnapshot {
            pending_start_offset: self.reader.pending_start_offset(),
            sync_state: self.sync_state,
            next_pts_samples: self.next_pts_samples,
            reached_end: self.reached_end,
        };
        match self.seek_to_unit_at_or_before(target) {
            Ok(result) => Ok(result),
            Err(original) => match self.restore(snapshot) {
                Ok(()) => Err(original),
                Err(rollback) => Err(Ac3ElementaryDemuxError::SeekRollbackFailed {
                    original: Box::new(original),
                    rollback: Box::new(rollback),
                }),
            },
        }
    }

    /// Ищет пакет не позже цели по оценке «байт на сэмпл», отступая назад при промахе.
    fn seek_to_unit_at_or_before(
        &mut self,
        target: Duration,
    ) -> Result<DemuxSeekResult, Ac3ElementaryDemuxError> {
        let target_samples = self.timeline.duration_to_samples(target);
        for backoff_step in 0..=MAX_SEEK_BACKOFF_STEPS {
            let search_start = self
                .timeline
                .seek_search_start(target_samples, backoff_step);
            let Some(landing_offset) = self.find_unit_from(search_start)? else {
                continue;
            };
            let landing_samples = self.timeline.samples_at_offset(landing_offset);
            if landing_samples <= target_samples {
                return self.commit_seek(target, landing_samples);
            }
        }
        // Оценка так и не дала точку до цели: начало данных всегда подходит.
        let data_start = self.timeline.data_start_offset();
        match self.find_unit_from(data_start)? {
            Some(_) => self.commit_seek(target, 0),
            None => Err(Ac3ElementaryDemuxError::SeekTargetUnavailable { target }),
        }
    }

    /// Абсолютное смещение первого целого пакета, начиная с `search_start`.
    ///
    /// Буфер остаётся спозиционированным на найденном пакете (мусор перед ним
    /// потреблён), поэтому успешный seek просто продолжает чтение отсюда.
    fn find_unit_from(
        &mut self,
        search_start: u64,
    ) -> Result<Option<u64>, Ac3ElementaryDemuxError> {
        self.reader.reposition(search_start)?;
        let mut discarded = 0_usize;
        let mut state = SyncState::Searching;
        while discarded <= self.options.max_resync_bytes.get() {
            let step = next_framing_step(self.reader.pending(), state, self.reader.continuation());
            state = step.sync_state_after(state);
            let discarded_now = match step {
                FramingStep::AccessUnit { skipped_bytes, .. } => {
                    self.reader.consume(skipped_bytes);
                    return Ok(Some(self.reader.pending_start_offset()));
                }
                FramingStep::Skip { bytes, .. } => bytes,
                FramingStep::NeedMoreData { discardable_bytes } => {
                    if self.reader.is_input_exhausted() {
                        return Ok(None);
                    }
                    self.reader.consume(discardable_bytes);
                    self.reader.read_more()?;
                    // Байты уже потреблены выше; ниже учитывается только их число.
                    discarded = discarded.saturating_add(discardable_bytes);
                    continue;
                }
                FramingStep::TruncatedTail { .. } => return Ok(None),
            };
            self.reader.consume(discarded_now);
            discarded = discarded.saturating_add(discarded_now);
        }
        Ok(None)
    }

    /// Фиксирует найденную точку (буфер уже стоит на ней): время, синхронизация, EOF.
    fn commit_seek(
        &mut self,
        target: Duration,
        landing_samples: u64,
    ) -> Result<DemuxSeekResult, Ac3ElementaryDemuxError> {
        self.sync_state = SyncState::Searching;
        self.next_pts_samples = landing_samples;
        self.discarded_without_unit = 0;
        self.reached_end = false;
        let actual = TrackTimestamp::from_unsigned_units(
            AUDIO_TRACK_ID,
            landing_samples,
            self.timeline.time_base(),
        );
        Ok(DemuxSeekResult {
            requested_position: MediaTime::from_duration(target),
            actual_position: actual.to_media_time(),
            actual_track_timestamp: Some(actual),
        })
    }

    /// Возвращает чтение в состояние до неудачного seek-а.
    fn restore(&mut self, snapshot: ReadPositionSnapshot) -> Result<(), Ac3ElementaryDemuxError> {
        self.reader.reposition(snapshot.pending_start_offset)?;
        self.sync_state = snapshot.sync_state;
        self.next_pts_samples = snapshot.next_pts_samples;
        self.reached_end = snapshot.reached_end;
        self.discarded_without_unit = 0;
        Ok(())
    }
}

impl Demuxer for Ac3ElementaryDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.timeline.estimated_duration()
    }

    fn seekability(&self) -> DemuxSeekability {
        if self.reader.is_seekable() {
            DemuxSeekability::Seekable
        } else {
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            }
        }
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        self.read_next_event().map_err(Into::into)
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_transactional(timestamp).map_err(Into::into)
    }

    /// Каждый access unit декодируется самостоятельно, поэтому все режимы
    /// означают одно: начало пакета не позже цели (дальше player делает pre-roll/drop).
    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        match request.mode {
            DemuxSeekMode::Accurate | DemuxSeekMode::DecodePointBefore | DemuxSeekMode::Preview => {
                self.seek_transactional(request.timestamp)
                    .map_err(Into::into)
            }
        }
    }
}

/// Пропускает ID3v2-теги в начале входа (с пределом на размер).
fn skip_leading_tags(
    reader: &mut ElementaryByteReader,
    options: Ac3ElementaryDemuxOptions,
) -> Result<(), Ac3ElementaryDemuxError> {
    for _ in 0..MAX_LEADING_TAGS {
        reader.fill_to(ID3V2_HEADER_BYTES)?;
        let LeadingTag::Id3v2 { total_bytes } = classify_leading_tag(reader.pending()) else {
            return Ok(());
        };
        let limit_bytes = options.max_leading_tag_bytes.get();
        if total_bytes > limit_bytes as u64 {
            return Err(Ac3ElementaryDemuxError::LeadingTagTooLarge {
                declared_bytes: total_bytes,
                limit_bytes,
            });
        }
        reader.skip(total_bytes)?;
    }
    Ok(())
}

/// Находит первый пакет окна и меряет «байт на сэмпл» по всем пакетам окна.
///
/// Буфер не потребляется: это пробный проход по уже прочитанным байтам.
fn scan_initial_window(
    reader: &ElementaryByteReader,
) -> Result<InitialScan, Ac3ElementaryDemuxError> {
    let window = reader.pending();
    let continuation = reader.continuation();
    let mut cursor = 0_usize;
    let mut state = SyncState::Searching;
    let mut scan: Option<InitialScan> = None;
    while cursor < window.len() {
        let step = next_framing_step(&window[cursor..], state, continuation);
        state = step.sync_state_after(state);
        match step {
            FramingStep::AccessUnit {
                skipped_bytes,
                unit,
            } => {
                let unit_start = cursor + skipped_bytes;
                let unit_end = unit_start + unit.total_length_bytes();
                let samples = u64::from(unit.samples_per_frame());
                match &mut scan {
                    None => {
                        scan = ByteRateEstimate::from_first_unit(
                            unit.total_length_bytes() as u64,
                            samples,
                        )
                        .map(|estimate| InitialScan {
                            first_unit: unit,
                            first_unit_buffer_offset: unit_start,
                            estimate,
                        });
                    }
                    Some(existing) => existing
                        .estimate
                        .add_unit(unit.total_length_bytes() as u64, samples),
                }
                cursor = unit_end;
            }
            FramingStep::Skip { bytes, .. } => cursor += bytes,
            FramingStep::NeedMoreData { .. } | FramingStep::TruncatedTail { .. } => break,
        }
    }
    scan.ok_or(Ac3ElementaryDemuxError::NoAccessUnit {
        scanned_bytes: window.len(),
    })
}

/// Описание дорожки по первому пакету.
fn track_info(unit: &AccessUnit, timeline: &StreamTimeline) -> TrackInfo {
    let codec_id = match unit.codec() {
        SyncframeCodec::Ac3 => AC3_CODEC_ID,
        SyncframeCodec::EnhancedAc3 => ENHANCED_AC3_CODEC_ID,
    };
    let duration = timeline.estimated_duration();
    TrackInfo {
        id: AUDIO_TRACK_ID,
        kind: TrackKind::Audio,
        codec_id: codec_id.to_owned(),
        codec_private: None,
        time_base: Some(timeline.time_base()),
        duration,
        sample_rate: Some(unit.primary_header().sample_rate_hz()),
        // Каналы основной программы с учётом dependent substream-ов (5.1 + Lrs/Rrs = 8).
        channels: Some(unit.presentation_channel_count()),
        video: None,
        labels: TrackLabels::default(),
    }
}
