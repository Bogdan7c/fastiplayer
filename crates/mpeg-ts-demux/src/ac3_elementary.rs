//! Нарезка AC-3 / E-AC-3 elementary stream-а на пакеты декодера.
//!
//! Заголовки и границы access unit-ов разбирает `ac3-syncframe`; здесь только
//! MPEG-TS-специфика: синхронизация с границами syncframe-ов и перевод access
//! unit-а в [`ElementaryPacket`]. Буфер PID-а и PTS принадлежат demuxer-у.
//!
//! В отличие от ADTS, syncframe-ы AC-3 в DVB регулярно режутся границами PES,
//! а после seek/сброса PID-а первый PES начинается с хвоста чужого кадра.
//! Поэтому до первого найденного syncframe-а мусор пропускается («поиск
//! синхронизации»).
//!
//! Потеря синхронизации посреди потока (битый заголовок после ошибки приёма,
//! мусор между кадрами) — тоже не конец воспроизведения: как у raw demuxer-а
//! (`ac3-elementary-demux`, `max_resync_bytes`), состояние возвращается к поиску
//! следующего проверенного syncframe-а. Предел — подряд отброшенные байты без
//! единого кадра (`MpegTsDemuxOptions::ac3_resync_bytes`); за ним поток считается
//! не-AC-3 и возвращается typed [`MpegTsDemuxError::Ac3SyncLost`]. Кадр после
//! пропуска помечается [`FrameContinuity::FollowsDiscardedBytes`]: длительность
//! пропущенного звука неизвестна, и его PTS может дать только новый PES.

use ac3_syncframe::{
    AccessUnit, AccessUnitScan, StreamContinuation, SyncframeCodec, SyncframeSearch,
    locate_syncframe, scan_access_unit,
};
use bytes::Bytes;
use media_core::{PacketDecodeStartInitialization, PacketKeyframe};
use tracing::warn;

use crate::MpegTsDemuxError;
use crate::elementary::{AudioFrameDrain, BufferedAudioFrame, ElementaryPacket, FrameContinuity};

/// Стабильные codec id, совпадающие с MKV/MP4 путём (`symphonia-demux` track mapper).
const AC3_CODEC_ID: &str = "A_AC3";
const ENHANCED_AC3_CODEC_ID: &str = "A_EAC3";

/// Частота MPEG system clock, в единицах которой считаются длительности пакетов.
const MPEG_CLOCK_HZ: u64 = 90_000;

/// Синхронизирован ли буфер PID-а с границами syncframe-ов.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum SyncframeAlignment {
    /// Начало потока, сброс PID-а или потеря синхронизации: байты до
    /// следующего проверенного syncframe-а пропускаются.
    #[default]
    SearchingSyncframe,
    /// Буфер начинается на границе syncframe-а.
    Locked,
}

/// Состояние синхронизации одного AC-3 PID-а между drain-ами (живёт в буфере PID-а).
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct Ac3SyncState {
    alignment: SyncframeAlignment,
    /// Байты, отброшенные подряд после последнего опубликованного кадра
    /// (через границы PES). Не ноль — следующий кадр идёт после пропуска.
    discarded_since_last_frame: usize,
    /// Сколько раз захваченная синхронизация терялась (диагностика в логе).
    lost_sync_count: u64,
}

impl Ac3SyncState {
    /// Учитывает пропущенные байты и обрывает бесконечный поиск синхронизации.
    fn note_discarded(
        &mut self,
        byte_count: usize,
        budget: Ac3ResyncBudget,
    ) -> Result<(), MpegTsDemuxError> {
        self.discarded_since_last_frame =
            self.discarded_since_last_frame.saturating_add(byte_count);
        if self.discarded_since_last_frame > budget.max_discarded_bytes {
            return Err(MpegTsDemuxError::Ac3SyncLost {
                pid: budget.pid,
                discarded_bytes: self.discarded_since_last_frame,
                limit_bytes: budget.max_discarded_bytes,
            });
        }
        Ok(())
    }

    /// Захваченная синхронизация потеряна на `reason`: дальше — поиск.
    fn lose_sync(&mut self, budget: Ac3ResyncBudget, reason: &dyn std::fmt::Display) {
        self.alignment = SyncframeAlignment::SearchingSyncframe;
        self.lost_sync_count = self.lost_sync_count.saturating_add(1);
        warn!(
            pid = budget.pid,
            lost_sync_count = self.lost_sync_count,
            %reason,
            "AC-3 elementary stream lost syncframe alignment; resynchronizing on the next verified syncframe"
        );
    }

    /// Непрерывность следующего кадра; публикация кадра обнуляет счётчик пропуска.
    fn take_frame_continuity(&mut self) -> FrameContinuity {
        let continuity = if self.discarded_since_last_frame == 0 {
            FrameContinuity::FollowsPreviousFrame
        } else {
            FrameContinuity::FollowsDiscardedBytes
        };
        self.discarded_since_last_frame = 0;
        continuity
    }
}

/// Предел поиска синхронизации для конкретного PID-а.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Ac3ResyncBudget {
    /// PID — для typed ошибки и лога.
    pub(crate) pid: u16,
    /// Сколько байт подряд можно отбросить без единого кадра.
    pub(crate) max_discarded_bytes: usize,
}

/// Достаёт из буфера все завершённые access unit-ы, оставляя незавершённый хвост.
///
/// `continuation == EndOfStream` закрывает последний E-AC-3 пакет (он иначе
/// ждёт следующего syncframe-а — вдруг там его dependent substream).
pub(crate) fn drain_ac3_access_units(
    payload: &[u8],
    sync: &mut Ac3SyncState,
    continuation: StreamContinuation,
    budget: Ac3ResyncBudget,
) -> Result<AudioFrameDrain, MpegTsDemuxError> {
    let mut cursor = 0_usize;
    let mut frames = Vec::new();
    while cursor < payload.len() {
        if sync.alignment == SyncframeAlignment::SearchingSyncframe {
            match locate_syncframe(&payload[cursor..]) {
                SyncframeSearch::Found { offset, .. } => {
                    sync.note_discarded(offset, budget)?;
                    cursor += offset;
                    sync.alignment = SyncframeAlignment::Locked;
                }
                // Мусор, который гарантированно не начинает кадр, отбрасывается;
                // хвост (возможная половина синхрослова/заголовка) ждёт данных.
                SyncframeSearch::NeedMoreData {
                    discardable_prefix_bytes,
                } => {
                    sync.note_discarded(discardable_prefix_bytes, budget)?;
                    cursor += discardable_prefix_bytes;
                    break;
                }
            }
        }
        let scan = match scan_access_unit(&payload[cursor..], continuation) {
            Ok(scan) => scan,
            // Оборванный кадр в конце потока: уже собранные пакеты публикуются,
            // а хвост остаётся в буфере — владелец буфера сообщит о нём как о
            // ragged frame (тот же контракт, что у ADTS).
            Err(error) if error.is_truncation() => break,
            // Битый заголовок/мусор на месте кадра: байт, на котором стоял
            // ожидаемый кадр, отбрасывается, и ищется следующий syncframe.
            Err(error) => {
                sync.lose_sync(budget, &error);
                sync.note_discarded(1, budget)?;
                cursor += 1;
                continue;
            }
        };
        match scan {
            AccessUnitScan::Complete(access_unit) => {
                let primary_program =
                    &payload[cursor..cursor + access_unit.primary_program_length_bytes()];
                frames.push(BufferedAudioFrame {
                    buffer_offset: cursor,
                    packet: packet_from_access_unit(&access_unit, primary_program),
                    continuity: sync.take_frame_continuity(),
                });
                cursor += access_unit.total_length_bytes();
            }
            AccessUnitScan::NeedMoreData => break,
            // Dependent substream без своего независимого (продолжение пакета,
            // начатого до seek/сброса или до повреждения): декодировать его не
            // с чем — пропускаем как потерянные данные.
            AccessUnitScan::OrphanSyncframe {
                syncframe_length_bytes,
                ..
            } => {
                sync.note_discarded(syncframe_length_bytes, budget)?;
                cursor += syncframe_length_bytes;
            }
        }
    }
    Ok(AudioFrameDrain {
        frames,
        consumed_bytes: cursor,
    })
}

/// Пакет декодера = основная программа access unit-а.
///
/// Дополнительные программы E-AC-3 (независимые substream-ы 1–7) — это
/// альтернативные звуковые программы, не каналы основной; декодер играет
/// одну программу, поэтому в пакет попадает только префикс основной.
fn packet_from_access_unit(access_unit: &AccessUnit, primary_program: &[u8]) -> ElementaryPacket {
    let header = access_unit.primary_header();
    let sample_rate = header.sample_rate_hz();
    let codec_id = match access_unit.codec() {
        SyncframeCodec::Ac3 => AC3_CODEC_ID,
        SyncframeCodec::EnhancedAc3 => ENHANCED_AC3_CODEC_ID,
    };
    ElementaryPacket {
        bytes: Bytes::copy_from_slice(primary_program),
        // Как и у остальных аудио PID-ов: аудио кадры не индексируются как
        // keyframe-ы, иначе seek index рос бы на каждый 32-мс кадр.
        keyframe: PacketKeyframe::NotKeyframe,
        decode_start_initialization: PacketDecodeStartInitialization::RequiresTrackConfiguration,
        sample_rate: Some(sample_rate),
        channels: Some(access_unit.presentation_channel_count()),
        audio_codec_id: Some(codec_id),
        duration_90khz: Some(
            u64::from(access_unit.samples_per_frame()).saturating_mul(MPEG_CLOCK_HZ)
                / u64::from(sample_rate),
        ),
    }
}
