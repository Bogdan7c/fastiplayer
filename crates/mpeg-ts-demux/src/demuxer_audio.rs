//! Аудио-половина `MpegTsDemuxer`: накопление PES payload-ов, нарезка на кадры
//! и публикация пакетов с per-frame PTS.
//!
//! Вынесено из `demuxer.rs` (лимит размера модуля), владелец состояния тот же —
//! `MpegTsDemuxer`; модуль лишь группирует методы аудио-пути.
//!
//! Таймстемпы (ISO/IEC 13818-1 §2.4.3.7): PES PTS относится к первому кадру,
//! который *начинается* в этом PES. Поэтому буфер PID-а помнит, с какой позиции
//! начался payload каждого PES («якорь»), и кадр получает PTS последнего якоря,
//! лежащего не дальше его первого байта. Кадр, начатый в предыдущем PES,
//! продолжает цепочку длительностей предыдущего кадра (carry-over).
//!
//! Это важно для AC-3/E-AC-3: пакет E-AC-3 ждёт заголовка следующего кадра,
//! поэтому буфер почти никогда не пуст на границе PES. Если бы PTS брался
//! только у PES, пришедшего в пустой буфер, таймлайн шёл бы одной цепочкой
//! усечённых длительностей и при 44.1 кГц уплывал бы на ~0.7 тика за кадр.

use ac3_syncframe::StreamContinuation;
use media_core::DemuxReadEvent;

use super::{AudioAccumulator, AudioTrackEvidence, MpegTsDemuxer};
use crate::MpegTsDemuxError;
use crate::ac3_elementary::{Ac3ResyncBudget, drain_ac3_access_units};
use crate::elementary::{
    AudioFrameDrain, ElementaryPacket, FrameContinuity, drain_adts_frames, drain_mpeg_audio_frames,
};
use crate::pes::PesPacket;
use crate::psi::StreamKind;
use crate::timeline_continuity::ElementaryClock;

/// Таймстемпы одного PES, payload которого лежит в буфере начиная с `buffer_offset`.
#[derive(Debug, Clone, Copy)]
pub(super) struct PesTimestampAnchor {
    buffer_offset: usize,
    pts: i64,
    dts: Option<i64>,
    byte_offset: u64,
}

/// Таймстемпы, назначенные конкретному кадру.
#[derive(Debug, Clone, Copy)]
struct FrameTiming {
    pts: i64,
    dts: Option<i64>,
    byte_offset: u64,
}

/// Можно ли продолжать цепочку длительностей от предыдущего кадра.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum TimingChain {
    /// Следующий кадр без своего PES PTS получает «PTS предыдущего + длительность».
    #[default]
    Continuous,
    /// Перед кадром отброшены байты неизвестной длительности (поиск
    /// синхронизации AC-3): кадры до следующего PES PTS не получают времени.
    AwaitingPesTimestamp,
}

/// Чем закончился поиск времени для кадра.
#[derive(Debug, Clone, Copy)]
enum FrameTimingLookup {
    /// Время кадра известно.
    Timed(FrameTiming),
    /// Кадр после пропуска до следующего PES PTS: публиковать его с выдуманным
    /// временем нельзя (рассинхрон), он отбрасывается.
    AwaitingPesTimestamp,
    /// Ни PES PTS, ни цепочки — нарушение формата.
    Missing,
}

impl AudioAccumulator {
    /// Добавляет payload PES и запоминает, с какой позиции действует его PTS.
    fn append_pes(&mut self, pes: &PesPacket, raw_pts: i64, raw_dts: Option<i64>) {
        self.pes_anchors.push_back(PesTimestampAnchor {
            buffer_offset: self.bytes.len(),
            pts: raw_pts,
            dts: raw_dts,
            byte_offset: pes.byte_offset,
        });
        self.bytes.extend_from_slice(&pes.payload);
    }

    /// PTS кадра, начинающегося на `buffer_offset`: якорь PES, начавшегося не
    /// позже кадра, иначе продолжение цепочки длительностей (если она не прервана).
    fn timing_for_frame_at(
        &mut self,
        buffer_offset: usize,
        continuity: FrameContinuity,
    ) -> FrameTimingLookup {
        if continuity == FrameContinuity::FollowsDiscardedBytes {
            self.timing_chain = TimingChain::AwaitingPesTimestamp;
        }
        while let Some(anchor) = self.pes_anchors.front().copied()
            && anchor.buffer_offset <= buffer_offset
        {
            self.pes_anchors.pop_front();
            self.next_pts = Some(anchor.pts);
            self.dts = anchor.dts;
            self.byte_offset = anchor.byte_offset;
            self.timing_chain = TimingChain::Continuous;
        }
        match (self.timing_chain, self.next_pts) {
            (TimingChain::AwaitingPesTimestamp, _) => FrameTimingLookup::AwaitingPesTimestamp,
            (TimingChain::Continuous, Some(pts)) => FrameTimingLookup::Timed(FrameTiming {
                pts,
                dts: self.dts,
                byte_offset: self.byte_offset,
            }),
            (TimingChain::Continuous, None) => FrameTimingLookup::Missing,
        }
    }

    fn advance_after_frame(&mut self, frame_pts: i64, duration_ticks: i64) {
        self.next_pts = Some(frame_pts.saturating_add(duration_ticks));
    }

    /// Отбрасывает разобранные байты и переводит якоря в новые координаты.
    ///
    /// Якорь внутри отброшенной части (PES начался посреди уже разобранного
    /// кадра) прижимается к нулю: его PTS достанется следующему кадру, который
    /// как раз и начался в этом PES.
    fn consume(&mut self, consumed_bytes: usize) {
        self.bytes.drain(..consumed_bytes);
        for anchor in &mut self.pes_anchors {
            anchor.buffer_offset = anchor.buffer_offset.saturating_sub(consumed_bytes);
        }
        // Из нескольких якорей, прижатых к нулю, действует только последний:
        // старые удаляются, иначе поток без кадров (поиск синхронизации по
        // мусору) копил бы якоря без предела.
        while self
            .pes_anchors
            .get(1)
            .is_some_and(|next_anchor| next_anchor.buffer_offset == 0)
        {
            self.pes_anchors.pop_front();
        }
        if self.bytes.is_empty() && self.pes_anchors.is_empty() {
            self.next_pts = None;
            self.dts = None;
        }
    }
}

impl MpegTsDemuxer {
    /// Добавляет payload аудио PES в буфер PID-а и публикует завершённые кадры.
    pub(super) fn process_audio_pes(
        &mut self,
        pes: &PesPacket,
        kind: StreamKind,
        raw_pts: i64,
        raw_dts: Option<i64>,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        let accumulator = self.audio_by_pid.entry(pes.pid).or_default();
        accumulator.append_pes(pes, raw_pts, raw_dts);
        let budget = Ac3ResyncBudget {
            pid: pes.pid,
            max_discarded_bytes: self.options.ac3_resync_bytes.get(),
        };
        let drain = drain_audio_frames(
            kind,
            accumulator,
            StreamContinuation::MoreBytesMayFollow,
            budget,
        )?;
        self.emit_audio_frames(pes.pid, kind, drain, publish_lifecycle)
    }

    /// На доказанном конце input-а/segment-а закрывает буферизованные кадры.
    ///
    /// E-AC-3 access unit ждёт следующего syncframe-а (вдруг это его dependent),
    /// поэтому последний пакет публикуется только здесь. Оставшийся после
    /// этого хвост любого кодека — оборванный кадр и typed `Malformed`.
    pub(super) fn finish_pending_audio(
        &mut self,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        let ac3_pids: Vec<u16> = self
            .audio_by_pid
            .iter()
            .filter(|(pid, accumulator)| {
                !accumulator.bytes.is_empty()
                    && self.stream_by_pid.get(pid) == Some(&StreamKind::Ac3Syncframes)
            })
            .map(|(pid, _)| *pid)
            .collect();
        for pid in ac3_pids {
            let budget = Ac3ResyncBudget {
                pid,
                max_discarded_bytes: self.options.ac3_resync_bytes.get(),
            };
            let Some(accumulator) = self.audio_by_pid.get_mut(&pid) else {
                continue;
            };
            let drain = drain_audio_frames(
                StreamKind::Ac3Syncframes,
                accumulator,
                StreamContinuation::EndOfStream,
                budget,
            )?;
            self.emit_audio_frames(pid, StreamKind::Ac3Syncframes, drain, publish_lifecycle)?;
        }
        if let Some((&pid, _)) = self
            .audio_by_pid
            .iter()
            .find(|(_, accumulator)| !accumulator.bytes.is_empty())
        {
            return Err(MpegTsDemuxError::Malformed {
                reason: format!("ragged elementary audio frame on PID {pid}"),
            });
        }
        Ok(())
    }

    fn emit_audio_frames(
        &mut self,
        pid: u16,
        kind: StreamKind,
        drain: AudioFrameDrain,
        publish_lifecycle: bool,
    ) -> Result<(), MpegTsDemuxError> {
        for frame in drain.frames {
            let lookup =
                self.audio_by_pid
                    .get_mut(&pid)
                    .map_or(FrameTimingLookup::Missing, |accumulator| {
                        accumulator.timing_for_frame_at(frame.buffer_offset, frame.continuity)
                    });
            let timing = match lookup {
                FrameTimingLookup::Timed(timing) => timing,
                // Пропуск уже залогирован при потере синхронизации; кадры до
                // следующего PES PTS — часть того же восстановимого повреждения.
                FrameTimingLookup::AwaitingPesTimestamp => continue,
                FrameTimingLookup::Missing => {
                    return Err(MpegTsDemuxError::Malformed {
                        reason: format!("audio frame without PES timestamp on PID {pid}"),
                    });
                }
            };
            let track_changed = self.update_audio_evidence(pid, kind, &frame.packet);
            if track_changed && publish_lifecycle {
                self.publish_tracks_changed();
            }
            // Цепочка длительностей (`advance_after_frame`) остаётся в raw-домене
            // PID-а; на шкалу презентации переносится только опубликованный кадр.
            let presented = self.present_on_timeline(
                pid,
                ElementaryClock::Audio,
                timing.pts,
                timing.dts,
                frame
                    .packet
                    .duration_90khz
                    .and_then(|duration| i64::try_from(duration).ok()),
                timing.byte_offset,
            );
            let packet = self.make_packet(
                pid,
                kind,
                presented.pts,
                presented.dts,
                timing.byte_offset,
                frame.packet,
            );
            if packet.keyframe.is_known_keyframe() {
                self.add_seek_anchor(presented.pts, timing.byte_offset, presented.epoch);
            }
            let packet_end = packet
                .duration
                .map_or(packet.pts, |duration| packet.pts.saturating_add(duration));
            self.duration = Some(self.duration.unwrap_or_default().max(packet_end));
            let duration_ticks = packet
                .track_duration
                .map_or(0, |duration| duration.units.get() as i64);
            if let Some(accumulator) = self.audio_by_pid.get_mut(&pid) {
                accumulator.advance_after_frame(timing.pts, duration_ticks);
            }
            self.pending_events
                .push_back(DemuxReadEvent::Packet(packet));
        }
        if let Some(accumulator) = self.audio_by_pid.get_mut(&pid) {
            accumulator.consume(drain.consumed_bytes);
        }
        Ok(())
    }

    /// Обновляет track evidence PID-а по кадру; `true` — дорожка изменилась
    /// и её надо опубликовать lifecycle-событием `TracksChanged`.
    fn update_audio_evidence(
        &mut self,
        pid: u16,
        kind: StreamKind,
        frame: &ElementaryPacket,
    ) -> bool {
        let Some(codec_id) = frame.audio_codec_id else {
            return false;
        };
        let evidence = AudioTrackEvidence {
            codec_id,
            sample_rate: frame.sample_rate,
            channels: frame.channels,
        };
        let previous = self.audio_evidence_by_pid.get(&pid).copied();
        if previous == Some(evidence) {
            return false;
        }
        if previous.is_some()
            && audio_evidence_policy(kind) == AudioEvidencePolicy::FirstFrameDefinesTrack
        {
            return false;
        }
        self.audio_evidence_by_pid.insert(pid, evidence);
        self.rebuild_tracks();
        true
    }
}

/// Как кадры elementary stream-а влияют на опубликованную дорожку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AudioEvidencePolicy {
    /// Любая смена codec/частоты/каналов в заголовке кадра — новая дорожка
    /// (`TracksChanged`): decoder ADTS/MPEG audio сконфигурирован под формат дорожки.
    FollowEveryFrame,
    /// Дорожку описывает первый кадр, дальнейшие смены формата — внутреннее
    /// дело decoder-а, lifecycle не публикуется.
    FirstFrameDefinesTrack,
}

/// Политика evidence для кодека PID-а.
///
/// AC-3 / E-AC-3: вещательный поток штатно переключает 2.0 ↔ 5.1 (реклама ↔
/// фильм) и может сменить частоту. `TracksChanged` в player-core — это
/// seek-подобный полный сброс (видео замирает до следующего IDR, выбор дорожки
/// возвращается к default), а decoder семейства сам декодирует каждый кадр в его
/// настоящем формате, и player-core пересобирает output по decoded spec-у без
/// потери A/V sync (`session/audio_spec_change.rs`). Поэтому смена формата не
/// должна выходить в lifecycle.
///
/// То же для смены AC-3 ↔ E-AC-3 на одном PID-е: decoder семейства выбирает
/// синтаксис по `bsid` каждого packet-а, пересоздавать его не нужно.
///
/// Метаданные дорожки при этом не обновляются «тихо»: player-core хранит
/// копию списка дорожек и сравнивает с ней следующий `TracksChanged`
/// (маркер seek-а с той же топологией не должен выглядеть сменой дорожек).
/// Тихая правка развела бы две копии, поэтому `sample_rate/channels` дорожки —
/// это формат первого кадра (как container metadata у MKV/MP4); настоящий
/// формат каждого кадра сообщает decoder.
fn audio_evidence_policy(kind: StreamKind) -> AudioEvidencePolicy {
    match kind {
        StreamKind::Ac3Syncframes => AudioEvidencePolicy::FirstFrameDefinesTrack,
        StreamKind::AacAdts | StreamKind::MpegAudio | StreamKind::H264 | StreamKind::H265 => {
            AudioEvidencePolicy::FollowEveryFrame
        }
    }
}

/// Нарезает буфер PID-а framing-ом его кодека (буфер не меняется).
fn drain_audio_frames(
    kind: StreamKind,
    accumulator: &mut AudioAccumulator,
    continuation: StreamContinuation,
    ac3_budget: Ac3ResyncBudget,
) -> Result<AudioFrameDrain, MpegTsDemuxError> {
    match kind {
        // ADTS и MPEG audio не ждут следующего кадра: конец кадра известен
        // из его собственного заголовка, поэтому continuation им не нужен.
        StreamKind::AacAdts => drain_adts_frames(&accumulator.bytes),
        StreamKind::MpegAudio => drain_mpeg_audio_frames(&accumulator.bytes),
        StreamKind::Ac3Syncframes => drain_ac3_access_units(
            &accumulator.bytes,
            &mut accumulator.ac3_sync,
            continuation,
            ac3_budget,
        ),
        StreamKind::H264 | StreamKind::H265 => Err(MpegTsDemuxError::Malformed {
            reason: "internal: video PID routed to audio framing".to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pes(payload: &[u8], byte_offset: u64) -> PesPacket {
        PesPacket {
            pid: 0x0102,
            pts: None,
            dts: None,
            payload: payload.to_vec(),
            byte_offset,
        }
    }

    #[test]
    fn frame_takes_pts_of_pes_it_starts_in_otherwise_carries_chain() {
        let mut accumulator = AudioAccumulator::default();
        accumulator.append_pes(&pes(&[0; 10], 0), 1_000, None);
        accumulator.append_pes(&pes(&[0; 10], 188), 5_000, Some(4_000));
        // Кадр с позиции 0 — в первом PES.
        let first = accumulator
            .timing_for_frame_at(0, FrameContinuity::FollowsPreviousFrame)
            .timed();
        assert_eq!((first.pts, first.byte_offset), (1_000, 0));
        accumulator.advance_after_frame(first.pts, 100);
        // Кадр с позиции 6 начался в первом PES — продолжение цепочки.
        let carried = accumulator
            .timing_for_frame_at(6, FrameContinuity::FollowsPreviousFrame)
            .timed();
        assert_eq!(carried.pts, 1_100);
        accumulator.advance_after_frame(carried.pts, 100);
        // Кадр с позиции 12 начался во втором PES — его PTS/DTS/offset.
        let anchored = accumulator
            .timing_for_frame_at(12, FrameContinuity::FollowsPreviousFrame)
            .timed();
        assert_eq!(
            (anchored.pts, anchored.dts, anchored.byte_offset),
            (5_000, Some(4_000), 188)
        );
    }

    #[test]
    fn consumed_anchors_are_clamped_and_collapsed_without_unbounded_growth() {
        let mut accumulator = AudioAccumulator::default();
        for index in 0..50_i64 {
            accumulator.append_pes(&pes(&[0xaa; 4], 0), index * 10, None);
            // Поиск синхронизации отбрасывает мусор целиком, кадров нет.
            accumulator.consume(4);
        }
        assert!(accumulator.pes_anchors.len() <= 1);
        assert!(accumulator.bytes.is_empty());

        // PES начался посреди уже разобранного кадра: его PTS получит следующий кадр.
        let mut accumulator = AudioAccumulator::default();
        accumulator.append_pes(&pes(&[0; 8], 0), 100, None);
        accumulator.append_pes(&pes(&[0; 8], 188), 900, None);
        let first = accumulator
            .timing_for_frame_at(0, FrameContinuity::FollowsPreviousFrame)
            .timed();
        accumulator.advance_after_frame(first.pts, 50);
        accumulator.consume(12);
        let next = accumulator
            .timing_for_frame_at(0, FrameContinuity::FollowsPreviousFrame)
            .timed();
        assert_eq!(next.pts, 900);
    }

    #[test]
    fn fully_consumed_buffer_forgets_carried_timestamps() {
        let mut accumulator = AudioAccumulator::default();
        accumulator.append_pes(&pes(&[0; 4], 0), 700, None);
        let timing = accumulator
            .timing_for_frame_at(0, FrameContinuity::FollowsPreviousFrame)
            .timed();
        accumulator.advance_after_frame(timing.pts, 10);
        accumulator.consume(4);
        assert!(matches!(
            accumulator.timing_for_frame_at(0, FrameContinuity::FollowsPreviousFrame),
            FrameTimingLookup::Missing
        ));
    }

    /// После пропуска байт кадры того же PES не получают выдуманный PTS
    /// цепочки, а следующий PES восстанавливает время.
    #[test]
    fn frames_after_discarded_bytes_wait_for_next_pes_timestamp() {
        let mut accumulator = AudioAccumulator::default();
        accumulator.append_pes(&pes(&[0; 30], 0), 1_000, None);
        accumulator.append_pes(&pes(&[0; 10], 188), 9_000, None);
        let first = accumulator
            .timing_for_frame_at(0, FrameContinuity::FollowsPreviousFrame)
            .timed();
        accumulator.advance_after_frame(first.pts, 100);
        assert!(matches!(
            accumulator.timing_for_frame_at(20, FrameContinuity::FollowsDiscardedBytes),
            FrameTimingLookup::AwaitingPesTimestamp
        ));
        // Следующий кадр в том же PES идёт сразу за предыдущим, но цепочка
        // уже прервана — время по-прежнему неизвестно.
        assert!(matches!(
            accumulator.timing_for_frame_at(25, FrameContinuity::FollowsPreviousFrame),
            FrameTimingLookup::AwaitingPesTimestamp
        ));
        let anchored = accumulator
            .timing_for_frame_at(30, FrameContinuity::FollowsPreviousFrame)
            .timed();
        assert_eq!(anchored.pts, 9_000);
    }

    impl FrameTimingLookup {
        fn timed(self) -> FrameTiming {
            match self {
                Self::Timed(timing) => timing,
                other => panic!("frame timing expected, got {other:?}"),
            }
        }
    }
}
