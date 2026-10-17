//! Целый syncframe AC-3: `syncinfo`, `bsi`, 6 × `audblk`, `auxdata`,
//! `errorcheck` (A/52 §5.3, §5.4.4–5.4.5).
//!
//! Главный инвариант модуля — учёт бит (§5.4.4.1): аудиоблоки читаются
//! только до начала хвоста кадра (`auxdatal`/`auxdatae`/`crcrsv`/`crc2`,
//! их позиции известны от конца кадра), а всё между концом аудио и хвостом —
//! это `auxbits`, длина которых `nauxbits` не может быть отрицательной.

use super::audblk::output::DecodedAudioBlock;
use super::audblk::state::FrameBlockState;
use super::audblk::{BlockContext, decode_audio_block};
use super::bsi::{BitStreamInfo, read_bsi};
use super::syncinfo::{BitstreamFamily, SyncInfo, parse_syncinfo};
use crate::bitstream::BitReader;
use crate::error::{Ac3DecodeError, AudioBlockIndex, BitstreamViolation, FrameSection};
use crate::spectral::dither::DitherGenerator;
use crate::tables::frame_size::SampleRateCode;

/// Аудиоблоков в syncframe-е AC-3 (A/52 §5.1).
pub(crate) const AUDIO_BLOCKS_PER_SYNCFRAME: usize = 6;
/// Новых сэмплов на канал в одном аудиоблоке (A/52 §5.1).
pub(crate) const SAMPLES_PER_AUDIO_BLOCK: usize = 256;
/// Битов в `errorcheck`: `crcrsv` (1) + `crc2` (16), Таблица 5.5.
const ERROR_CHECK_BITS: usize = 17;
/// Ширина `auxdatae` и `auxdatal` (Таблица 5.4).
const AUX_DATA_EXISTS_BITS: usize = 1;
const AUX_DATA_LENGTH_BITS: u32 = 14;

/// Сводка заголовка syncframe-а AC-3.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct SyncframeInfo {
    /// Частота дискретизации с учётом пониженной частоты `bsid` 9/10.
    pub sample_rate_hz: u32,
    /// `fscod` (Таблица 5.6) — номинальная частота таблиц.
    pub sample_rate_code: SampleRateCode,
    /// `frmsizecod` (Таблица 5.18).
    pub frame_size_code: u8,
    /// Битрейт, кбит/с: номинал Таблицы 5.18, делённый на сдвиг частоты bsid 9/10
    /// (кадр той же длины звучит вдвое/вчетверо дольше).
    pub bitrate_kbps: u16,
    /// Длина кадра в байтах.
    pub frame_length_bytes: usize,
    /// Сэмплов на канал в кадре (6 × 256).
    pub samples_per_channel: usize,
    /// Информация о потоке.
    pub bsi: BitStreamInfo,
}

/// Учёт хвоста кадра после шести аудиоблоков (§5.4.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FrameTrailer {
    /// Позиция (в битах от начала кадра), где закончились данные аудиоблоков.
    pub(crate) audio_end_bits: usize,
    /// `nauxbits`: биты между концом аудио и полями `auxdatal`/`auxdatae`.
    pub(crate) auxiliary_bits: usize,
    /// `auxdatal`, если `auxdatae = 1`.
    pub(crate) user_data_bits: Option<usize>,
}

/// Разобранный кадр: заголовок и учёт хвоста; коэффициенты — в буфере блоков.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DecodedSyncframe {
    pub(crate) info: SyncframeInfo,
    pub(crate) trailer: FrameTrailer,
}

/// Разбирает `syncinfo` + `bsi`; возвращает сводку и читатель после BSI.
fn read_header(frame: &[u8]) -> Result<(SyncInfo, SyncframeInfo, BitReader<'_>), Ac3DecodeError> {
    let (syncinfo, family) = parse_syncinfo(frame)?;
    let BitstreamFamily::Ac3 { sample_rate_shift } = family else {
        return Err(Ac3DecodeError::Unsupported(
            crate::error::UnsupportedFeature::EnhancedAc3,
        ));
    };
    let frame_bytes = frame.get(..syncinfo.frame_bytes()).unwrap_or(frame);
    let mut reader = BitReader::new(frame_bytes);
    // syncword (16) + crc1 (16) + fscod (2) + frmsizecod (6) уже разобраны.
    const SYNCINFO_BITS: usize = 40;
    let overrun = |_| Ac3DecodeError::BitstreamOverrun {
        section: FrameSection::BitStreamInformation,
    };
    reader.skip_bits(SYNCINFO_BITS).map_err(overrun)?;
    let bsi = read_bsi(&mut reader).map_err(overrun)?;
    let info = SyncframeInfo {
        sample_rate_hz: syncinfo.sample_rate_code.hertz() >> sample_rate_shift,
        sample_rate_code: syncinfo.sample_rate_code,
        frame_size_code: syncinfo.frame_size_code,
        bitrate_kbps: syncinfo.nominal_bitrate_kbps >> sample_rate_shift,
        frame_length_bytes: syncinfo.frame_bytes(),
        samples_per_channel: AUDIO_BLOCKS_PER_SYNCFRAME * SAMPLES_PER_AUDIO_BLOCK,
        bsi,
    };
    Ok((syncinfo, info, reader))
}

/// Разбирает только заголовок кадра (`syncinfo` + `bsi`) без CRC и аудио.
pub(crate) fn read_syncframe_info(frame: &[u8]) -> Result<SyncframeInfo, Ac3DecodeError> {
    read_header(frame).map(|(_, info, _)| info)
}

/// Длина кадра в 16-битных словах по его заголовку (для проверки CRC).
pub(crate) fn frame_words(frame: &[u8]) -> Result<usize, Ac3DecodeError> {
    parse_syncinfo(frame).map(|(syncinfo, _)| syncinfo.frame_words)
}

/// Разбирает весь кадр AC-3 до частотных коэффициентов всех шести блоков.
pub(crate) fn decode_ac3_syncframe(
    frame: &[u8],
    dither: &mut DitherGenerator,
    blocks: &mut [DecodedAudioBlock; AUDIO_BLOCKS_PER_SYNCFRAME],
) -> Result<DecodedSyncframe, Ac3DecodeError> {
    let (syncinfo, info, mut reader) = read_header(frame)?;
    let frame_bits = syncinfo.frame_bytes() * 8;
    let trailer_layout = read_trailer_layout(frame, frame_bits)?;
    if reader.position_bits() > trailer_layout.audio_limit_bits {
        return Err(Ac3DecodeError::BitstreamOverrun {
            section: FrameSection::BitStreamInformation,
        });
    }
    reader.restrict_limit(trailer_layout.audio_limit_bits);
    let mut state = FrameBlockState::new();
    for (index, block) in blocks.iter_mut().enumerate() {
        let context = BlockContext {
            mode: info.bsi.audio_coding_mode,
            channel_count: info.bsi.full_bandwidth_channels(),
            lfe_on: info.bsi.lfe_on,
            sample_rate: info.sample_rate_code,
            block: AudioBlockIndex::new(index as u8),
        };
        decode_audio_block(&mut reader, &context, &mut state, dither, block)?;
    }
    let trailer = account_auxiliary_data(reader.position_bits(), trailer_layout)?;
    Ok(DecodedSyncframe { info, trailer })
}

/// Позиции полей хвоста, отсчитанные от конца кадра (Таблицы 5.4, 5.5).
///
/// Хвост E-AC-3 устроен так же (Annex E Таблицы E1.5–E1.6: `auxdatal`,
/// `auxdatae`, `encinfo` вместо `crcrsv`, `crc2`), поэтому разбор общий.
#[derive(Debug, Clone, Copy)]
pub(crate) struct TrailerLayout {
    /// Первая позиция, которую аудиоданные занимать уже не могут.
    pub(crate) audio_limit_bits: usize,
    /// `auxdatal`, если `auxdatae = 1`.
    user_data_bits: Option<usize>,
}

pub(crate) fn read_trailer_layout(
    frame: &[u8],
    frame_bits: usize,
) -> Result<TrailerLayout, Ac3DecodeError> {
    let overrun = |_| Ac3DecodeError::BitstreamOverrun {
        section: FrameSection::FrameTrailer,
    };
    let exists_position = frame_bits
        .checked_sub(ERROR_CHECK_BITS + AUX_DATA_EXISTS_BITS)
        .ok_or(Ac3DecodeError::BitstreamOverrun {
            section: FrameSection::FrameTrailer,
        })?;
    let mut reader = BitReader::new(frame);
    reader.skip_bits(exists_position).map_err(overrun)?;
    if !reader.read_flag().map_err(overrun)? {
        return Ok(TrailerLayout {
            audio_limit_bits: exists_position,
            user_data_bits: None,
        });
    }
    // auxdatal стоит перед auxdatae (Таблица 5.4: «if (auxdatae) {auxdatal}; auxdatae»).
    let length_position = exists_position
        .checked_sub(AUX_DATA_LENGTH_BITS as usize)
        .ok_or(Ac3DecodeError::BitstreamOverrun {
            section: FrameSection::FrameTrailer,
        })?;
    let mut length_reader = BitReader::new(frame);
    length_reader.skip_bits(length_position).map_err(overrun)?;
    let user_data_bits = usize::from(
        length_reader
            .read_bits(AUX_DATA_LENGTH_BITS)
            .map_err(overrun)?,
    );
    Ok(TrailerLayout {
        audio_limit_bits: length_position,
        user_data_bits: Some(user_data_bits),
    })
}

/// `nauxbits = (биты кадра) − (биты всех элементов, кроме auxbits)` (§5.4.4.1).
pub(crate) fn account_auxiliary_data(
    audio_end_bits: usize,
    layout: TrailerLayout,
) -> Result<FrameTrailer, Ac3DecodeError> {
    // Читатель аудиоблоков ограничен audio_limit_bits, поэтому заход аудио в
    // хвост уже стал бы overrun-ом блока (§7.10.2, условие 21); здесь
    // nauxbits ≥ 0 по построению, а checked_sub лишь не даёт этому сломаться молча.
    let auxiliary_bits = layout.audio_limit_bits.checked_sub(audio_end_bits).ok_or(
        Ac3DecodeError::BitstreamOverrun {
            section: FrameSection::FrameTrailer,
        },
    )?;
    if let Some(declared_bits) = layout.user_data_bits
        && declared_bits > auxiliary_bits
    {
        return Err(Ac3DecodeError::InvalidBitstream {
            section: FrameSection::FrameTrailer,
            violation: BitstreamViolation::AuxiliaryDataTooLong {
                declared_bits,
                available_bits: auxiliary_bits,
            },
        });
    }
    Ok(FrameTrailer {
        audio_end_bits,
        auxiliary_bits,
        user_data_bits: layout.user_data_bits,
    })
}
