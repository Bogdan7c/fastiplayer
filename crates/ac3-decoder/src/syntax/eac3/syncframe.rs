//! Целый syncframe E-AC-3: `syncinfo`, `bsi`, `audfrm`, 1/2/3/6 × `audblk`,
//! `auxdata`, `errorcheck` (A/52 Annex E §2.2; ETSI TS 102 366 v1.4.1 §E.1.2).
//!
//! Инвариант учёта бит тот же, что в AC-3 (`syntax::syncframe`): блоки
//! читаются только до начала хвоста, всё между концом аудио и хвостом —
//! `auxbits`, `nauxbits ≥ 0`, `auxdatal ≤ nauxbits`.

use super::audblk::mantissas::AhtFrameStore;
use super::audblk::state::EnhancedBlockState;
use super::audblk::{BlockResources, EnhancedBlockContext, decode_enhanced_audio_block};
use super::audfrm::{AudioFrame, AudioFrameContext, AudioFrameSummary, read_audio_frame};
use super::bsi::{
    EnhancedBitStreamInfo, EnhancedFrameHeader, StreamType, peek_frame_header, read_enhanced_bsi,
};
use super::tool_usage::EnhancedToolUsage;
use crate::bitstream::BitReader;
use crate::bitstream::crc::crc16_register;
use crate::error::{Ac3DecodeError, AudioBlockIndex, CrcRegion, FrameSection};
use crate::spectral::dither::DitherGenerator;
use crate::spectral::spx::SpxNoiseGenerator;
use crate::syntax::audblk::BlockContext;
use crate::syntax::audblk::output::DecodedAudioBlock;
use crate::syntax::syncframe::{
    AUDIO_BLOCKS_PER_SYNCFRAME, FrameTrailer, account_auxiliary_data, read_trailer_layout,
};
use crate::tables::frame_size::SampleRateCode;

/// Биты кадра до `acmod`: syncword (16) + strmtyp/substreamid/frmsiz (16) +
/// fscod/fscod2-numblkscod (4).
const BITS_BEFORE_CODING_MODE: usize = 36;
/// Синхрослово не входит в CRC (Annex E §3.2: CRC «covers the entire syncframe»
/// после синхрослова, как и crc1/crc2 AC-3).
const SYNC_WORD_BYTES: usize = 2;

/// Сводка заголовка syncframe-а E-AC-3.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct EnhancedSyncframeInfo {
    /// `strmtyp`.
    pub stream_type: StreamType,
    /// `substreamid` (0–7).
    pub substream_id: u8,
    /// Длина кадра в байтах (`(frmsiz + 1) · 2`).
    pub frame_length_bytes: usize,
    /// Частота дискретизации (с учётом пониженных частот `fscod2`).
    pub sample_rate_hz: u32,
    /// Строка частотных таблиц (`fscod`, а при `fscod = 3` — `fscod2`).
    pub sample_rate_code: SampleRateCode,
    /// Пониженная частота (`fscod = 3`, Таблица E2.3).
    pub reduced_sample_rate: bool,
    /// Аудиоблоков в кадре (1, 2, 3 или 6).
    pub audio_blocks: usize,
    /// Сэмплов на канал в кадре (256 × блоков).
    pub samples_per_channel: usize,
    /// Информация о потоке.
    pub bsi: EnhancedBitStreamInfo,
    /// Инструменты уровня кадра (`audfrm`).
    pub audio_frame: AudioFrameSummary,
}

/// Разобранный кадр: заголовок и учёт хвоста; коэффициенты — в буфере блоков.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedEnhancedSyncframe {
    pub(crate) info: EnhancedSyncframeInfo,
    pub(crate) trailer: FrameTrailer,
}

/// Состояние декодера, которое E-AC-3 использует между кадрами.
pub(crate) struct EnhancedFrameResources<'a> {
    pub(crate) aht: &'a mut AhtFrameStore,
    pub(crate) dither: &'a mut DitherGenerator,
    pub(crate) spx_noise: &'a mut SpxNoiseGenerator,
}

/// Заголовок, BSI и `audfrm`; читатель после `audfrm`.
fn read_header(
    frame: &[u8],
) -> Result<
    (
        EnhancedFrameHeader,
        EnhancedSyncframeInfo,
        AudioFrame,
        BitReader<'_>,
    ),
    Ac3DecodeError,
> {
    let header = peek_frame_header(frame)?;
    let frame = frame
        .get(..header.frame_bytes)
        .ok_or(Ac3DecodeError::Truncated {
            available_bytes: frame.len(),
            required_bytes: header.frame_bytes,
        })?;
    let mut reader = BitReader::new(frame);
    reader
        .skip_bits(BITS_BEFORE_CODING_MODE)
        .map_err(|_| Ac3DecodeError::BitstreamOverrun {
            section: FrameSection::BitStreamInformation,
        })?;
    let bsi = read_enhanced_bsi(&mut reader, &header)?;
    let audio_frame = read_audio_frame(
        &mut reader,
        &AudioFrameContext {
            audio_blocks: header.audio_blocks,
            channel_count: bsi.full_bandwidth_channels(),
            coupling_possible: bsi.audio_coding_mode.code() > 1,
            lfe_on: bsi.lfe_on,
            independent: header.stream_type == StreamType::Independent,
            frame_words: header.frame_bytes / 2,
        },
    )?;
    let info = EnhancedSyncframeInfo {
        stream_type: header.stream_type,
        substream_id: header.substream_id,
        frame_length_bytes: header.frame_bytes,
        sample_rate_hz: header.sample_rate_hz,
        sample_rate_code: header.sample_rate_code,
        reduced_sample_rate: header.reduced_sample_rate,
        audio_blocks: header.audio_blocks,
        samples_per_channel: header.samples_per_channel(),
        bsi,
        audio_frame: audio_frame.summary,
    };
    Ok((header, info, audio_frame, reader))
}

/// Разбирает заголовок кадра E-AC-3 (`bsi` + `audfrm`) без CRC и аудиоблоков.
pub(crate) fn read_enhanced_syncframe_info(
    frame: &[u8],
) -> Result<EnhancedSyncframeInfo, Ac3DecodeError> {
    read_header(frame).map(|(_, info, _, _)| info)
}

/// Проверяет единственную CRC кадра E-AC-3 (`crc2`, Annex E §3.2): регистр
/// после прогона кадра без синхрослова обязан обнулиться (§7.10.1).
pub(crate) fn verify_enhanced_crc(frame: &[u8]) -> Result<(), Ac3DecodeError> {
    let protected = frame.get(SYNC_WORD_BYTES..).unwrap_or_default();
    if crc16_register(protected) == 0 {
        Ok(())
    } else {
        Err(Ac3DecodeError::CrcMismatch {
            region: CrcRegion::WholeFrame,
        })
    }
}

/// Разбирает кадр E-AC-3 до частотных коэффициентов всех его блоков
/// (действительны первые `info.audio_blocks` элементов `blocks`).
pub(crate) fn decode_eac3_syncframe(
    frame: &[u8],
    resources: EnhancedFrameResources<'_>,
    blocks: &mut [DecodedAudioBlock; AUDIO_BLOCKS_PER_SYNCFRAME],
    usage: &mut EnhancedToolUsage,
) -> Result<DecodedEnhancedSyncframe, Ac3DecodeError> {
    let (header, info, audio_frame, mut reader) = read_header(frame)?;
    let frame = frame.get(..header.frame_bytes).unwrap_or(frame);
    let frame_bits = header.frame_bytes * 8;
    let trailer_layout = read_trailer_layout(frame, frame_bits)?;
    if reader.position_bits() > trailer_layout.audio_limit_bits {
        return Err(Ac3DecodeError::BitstreamOverrun {
            section: FrameSection::AudioFrame,
        });
    }
    reader.restrict_limit(trailer_layout.audio_limit_bits);
    usage.record_frame(&audio_frame);
    let EnhancedFrameResources {
        aht,
        dither,
        spx_noise,
    } = resources;
    let mut state = EnhancedBlockState::new();
    for (index, block) in blocks.iter_mut().enumerate().take(header.audio_blocks) {
        let context = EnhancedBlockContext {
            base: BlockContext {
                mode: info.bsi.audio_coding_mode,
                channel_count: info.bsi.full_bandwidth_channels(),
                lfe_on: info.bsi.lfe_on,
                sample_rate: header.sample_rate_code,
                block: AudioBlockIndex::new(index as u8),
            },
            frame: &audio_frame,
            independent: header.stream_type == StreamType::Independent,
        };
        let resources = BlockResources {
            state: &mut state,
            aht,
            dither,
            spx_noise,
            usage,
        };
        decode_enhanced_audio_block(&mut reader, &context, resources, block)?;
    }
    let trailer = account_auxiliary_data(reader.position_bits(), trailer_layout)?;
    Ok(DecodedEnhancedSyncframe { info, trailer })
}
