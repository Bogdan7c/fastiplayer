//! Типизированный заголовок одного syncframe-а AC-3 / E-AC-3.

use crate::SyncframeHeaderError;
use crate::bit_reader::BitReader;
use crate::channels::{ChannelLocations, coding_mode_locations};
use crate::search::SYNC_WORD;
use crate::tables::{
    AC3_AUDIO_BLOCKS_PER_SYNCFRAME, SAMPLES_PER_AUDIO_BLOCK, ac3_frame_length_bytes,
    audio_blocks_from_numblkscod, reduced_sample_rate_from_fscod2, sample_rate_from_fscod,
};

/// Последний `bsid` полноскоростного AC-3 (A/52 §5.4.2.1).
const FULL_RATE_AC3_BITSTREAM_ID: u8 = 8;
/// Последний `bsid` AC-3 синтаксиса: 9 и 10 — AC-3 пониженной частоты
/// (half/quarter rate; так их читают FFmpeg `ac3_parser` и собственный
/// `ac3-decoder`). Синтаксис кадра и таблица длин те же, частота — `fs >> (bsid − 8)`.
const MAX_AC3_BITSTREAM_ID: u8 = 10;
/// Диапазон `bsid` E-AC-3: 16 — текущая версия, 11–15 — обратно-совместимые
/// расширения, которые декодер версии 16 обязан декодировать (A/52 §E.1.3.1.6).
const EAC3_BITSTREAM_IDS: std::ops::RangeInclusive<u8> = 11..=16;
/// `bsid` лежит в старших 5 битах байта 5 в обоих синтаксисах.
const BITSTREAM_ID_BYTE_OFFSET: usize = 5;
/// Сколько байт нужно, чтобы вообще узнать `bsid`.
const BITSTREAM_ID_PROBE_BYTES: usize = BITSTREAM_ID_BYTE_OFFSET + 1;
/// `fscod == 3` у E-AC-3 означает «частота в `fscod2`, блоков всегда 6».
const EAC3_REDUCED_RATE_FSCOD: u8 = 3;

/// Кодек, к которому относится syncframe (по `bsid`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncframeCodec {
    /// AC-3 / Dolby Digital, `bsid` 0–8 (и 9–10 — пониженная частота).
    Ac3,
    /// E-AC-3 / Dolby Digital Plus, `bsid` 11–16.
    EnhancedAc3,
}

/// Режим кодирования full-bandwidth каналов (`acmod`, A/52 таблица 5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioCodingMode {
    /// 1+1: два независимых моно-канала.
    DualMono,
    /// 1/0: центр.
    Mono,
    /// 2/0: L, R.
    Stereo,
    /// 3/0: L, C, R.
    ThreeFront,
    /// 2/1: L, R, S.
    TwoFrontOneSurround,
    /// 3/1: L, C, R, S.
    ThreeFrontOneSurround,
    /// 2/2: L, R, Ls, Rs.
    TwoFrontTwoSurround,
    /// 3/2: L, C, R, Ls, Rs.
    ThreeFrontTwoSurround,
}

impl AudioCodingMode {
    fn from_acmod(acmod: u8) -> Self {
        match acmod {
            0 => Self::DualMono,
            1 => Self::Mono,
            2 => Self::Stereo,
            3 => Self::ThreeFront,
            4 => Self::TwoFrontOneSurround,
            5 => Self::ThreeFrontOneSurround,
            6 => Self::TwoFrontTwoSurround,
            // acmod — 3-битное поле, 7 — последнее значение.
            _ => Self::ThreeFrontTwoSurround,
        }
    }

    /// Исходное 3-битное значение `acmod`.
    #[must_use]
    pub const fn acmod(self) -> u8 {
        match self {
            Self::DualMono => 0,
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::ThreeFront => 3,
            Self::TwoFrontOneSurround => 4,
            Self::ThreeFrontOneSurround => 5,
            Self::TwoFrontTwoSurround => 6,
            Self::ThreeFrontTwoSurround => 7,
        }
    }

    /// Число full-bandwidth каналов (`nfchans`), без LFE.
    #[must_use]
    pub const fn full_bandwidth_channel_count(self) -> u32 {
        match self {
            Self::Mono => 1,
            Self::DualMono | Self::Stereo => 2,
            Self::ThreeFront | Self::TwoFrontOneSurround => 3,
            Self::ThreeFrontOneSurround | Self::TwoFrontTwoSurround => 4,
            Self::ThreeFrontTwoSurround => 5,
        }
    }

    const fn has_center_mix_level(self) -> bool {
        // cmixlev есть, когда есть три фронтальных канала (A/52 §5.4.2.4).
        self.acmod() & 0b001 != 0 && self.acmod() != 1
    }

    const fn has_surround_mix_level(self) -> bool {
        // surmixlev есть, когда есть хотя бы один surround (A/52 §5.4.2.5).
        self.acmod() & 0b100 != 0
    }
}

/// Есть ли в syncframe-е канал низкочастотных эффектов (`lfeon`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LowFrequencyEffects {
    /// LFE-канал присутствует (`.1` в «5.1»).
    Present,
    /// LFE-канала нет.
    Absent,
}

impl LowFrequencyEffects {
    const fn from_flag(lfeon: bool) -> Self {
        if lfeon { Self::Present } else { Self::Absent }
    }

    /// Сколько каналов добавляет LFE (0 или 1).
    #[must_use]
    pub const fn channel_count(self) -> u32 {
        match self {
            Self::Present => 1,
            Self::Absent => 0,
        }
    }
}

/// Тип substream-а E-AC-3 (`strmtyp`, A/52 таблица E2.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Eac3StreamType {
    /// Тип 0: независимый substream (самостоятельная программа).
    Independent,
    /// Тип 1: зависимый substream — дополняет каналы предыдущего независимого.
    Dependent,
    /// Тип 2: независимый substream, конвертированный из AC-3.
    Ac3Convertible,
}

/// Номер substream-а E-AC-3 (`substreamid`, 0–7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubstreamId(u8);

impl SubstreamId {
    /// Substream 0 — основная программа, с которой начинается каждый access unit.
    pub const PRIMARY: Self = Self(0);

    /// Числовое значение 0–7.
    #[must_use]
    pub const fn value(self) -> u8 {
        self.0
    }
}

/// Откуда берётся раскладка каналов dependent substream-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DependentChannelMap {
    /// `chanmape == 0`: каналы описаны собственными `acmod`/`lfeon`.
    FromCodingMode,
    /// `chanmape == 1`: явная 16-битная карта позиций `chanmap`.
    Explicit(ChannelLocations),
}

/// Поля, которые есть только у E-AC-3 syncframe-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Eac3Substream {
    stream_type: Eac3StreamType,
    substream_id: SubstreamId,
    channel_map: DependentChannelMap,
}

impl Eac3Substream {
    /// Тип substream-а.
    #[must_use]
    pub const fn stream_type(self) -> Eac3StreamType {
        self.stream_type
    }

    /// Номер substream-а.
    #[must_use]
    pub const fn substream_id(self) -> SubstreamId {
        self.substream_id
    }

    /// Раскладка каналов dependent substream-а; у независимых всегда `FromCodingMode`.
    #[must_use]
    pub const fn channel_map(self) -> DependentChannelMap {
        self.channel_map
    }

    /// Независимый (или AC-3-конвертируемый) substream — начало программы.
    #[must_use]
    pub const fn is_independent(self) -> bool {
        matches!(
            self.stream_type,
            Eac3StreamType::Independent | Eac3StreamType::Ac3Convertible
        )
    }
}

/// Синтаксис syncframe-а: AC-3 или E-AC-3 со своими substream-полями.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncframeLayout {
    /// Классический AC-3 syncframe (всегда самостоятельная программа).
    Ac3,
    /// E-AC-3 syncframe одного substream-а.
    EnhancedAc3(Eac3Substream),
}

/// Разобранный заголовок одного syncframe-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SyncframeHeader {
    layout: SyncframeLayout,
    bitstream_id: u8,
    frame_length_bytes: usize,
    sample_rate_hz: u32,
    samples_per_frame: u32,
    coding_mode: AudioCodingMode,
    low_frequency_effects: LowFrequencyEffects,
}

impl SyncframeHeader {
    /// AC-3 или E-AC-3.
    #[must_use]
    pub const fn codec(&self) -> SyncframeCodec {
        match self.layout {
            SyncframeLayout::Ac3 => SyncframeCodec::Ac3,
            SyncframeLayout::EnhancedAc3(_) => SyncframeCodec::EnhancedAc3,
        }
    }

    /// Синтаксис вместе с E-AC-3 substream-полями.
    #[must_use]
    pub const fn layout(&self) -> SyncframeLayout {
        self.layout
    }

    /// Substream-поля E-AC-3; у AC-3 их нет.
    #[must_use]
    pub const fn eac3_substream(&self) -> Option<Eac3Substream> {
        match self.layout {
            SyncframeLayout::Ac3 => None,
            SyncframeLayout::EnhancedAc3(substream) => Some(substream),
        }
    }

    /// Значение `bsid`.
    #[must_use]
    pub const fn bitstream_id(&self) -> u8 {
        self.bitstream_id
    }

    /// Полная длина syncframe-а в байтах, включая синхрослово.
    #[must_use]
    pub const fn frame_length_bytes(&self) -> usize {
        self.frame_length_bytes
    }

    /// Частота дискретизации, Гц.
    #[must_use]
    pub const fn sample_rate_hz(&self) -> u32 {
        self.sample_rate_hz
    }

    /// Сэмплов на канал в этом syncframe-е: 1536 у AC-3, `numblks × 256` у E-AC-3.
    #[must_use]
    pub const fn samples_per_frame(&self) -> u32 {
        self.samples_per_frame
    }

    /// Режим full-bandwidth каналов.
    #[must_use]
    pub const fn audio_coding_mode(&self) -> AudioCodingMode {
        self.coding_mode
    }

    /// Есть ли LFE-канал.
    #[must_use]
    pub const fn low_frequency_effects(&self) -> LowFrequencyEffects {
        self.low_frequency_effects
    }

    /// Число каналов, закодированных в этом syncframe-е (`nfchans + lfeon`).
    ///
    /// Для итоговой раскладки программы с dependent substream-ами используйте
    /// [`crate::AccessUnit::presentation_channel_count`].
    #[must_use]
    pub const fn channel_count(&self) -> u32 {
        self.coding_mode.full_bandwidth_channel_count() + self.low_frequency_effects.channel_count()
    }

    /// Позиции каналов этого syncframe-а в нотации chanmap.
    #[must_use]
    pub fn channel_locations(&self) -> ChannelLocations {
        if let SyncframeLayout::EnhancedAc3(substream) = self.layout
            && let DependentChannelMap::Explicit(locations) = substream.channel_map
        {
            return locations;
        }
        ChannelLocations::from_coding_mode(
            coding_mode_locations(self.coding_mode.acmod()),
            self.low_frequency_effects == LowFrequencyEffects::Present,
        )
    }

    /// Может ли syncframe начать access unit: AC-3 кадр или независимый
    /// substream 0 E-AC-3. Dependent substream-ы и программы 1–7 — нет.
    #[must_use]
    pub const fn starts_access_unit(&self) -> bool {
        match self.layout {
            SyncframeLayout::Ac3 => true,
            SyncframeLayout::EnhancedAc3(substream) => {
                substream.is_independent() && substream.substream_id.0 == SubstreamId::PRIMARY.0
            }
        }
    }
}

/// Разбирает заголовок syncframe-а, который начинается ровно с первого байта `bytes`.
///
/// Нужны только байты заголовка (до 12), не весь кадр: проверить, что кадр
/// целиком доступен, вызывающий может по [`SyncframeHeader::frame_length_bytes`].
///
/// # Errors
///
/// [`SyncframeHeaderError::Truncated`] — байтов пока мало (можно дочитать);
/// остальные варианты — здесь нет пригодного syncframe-а.
pub fn parse_syncframe_header(bytes: &[u8]) -> Result<SyncframeHeader, SyncframeHeaderError> {
    if bytes.len() < SYNC_WORD.len() {
        return Err(SyncframeHeaderError::Truncated {
            available_bytes: bytes.len(),
            required_bytes: SYNC_WORD.len(),
        });
    }
    if bytes[..SYNC_WORD.len()] != SYNC_WORD {
        return Err(SyncframeHeaderError::MissingSyncWord);
    }
    let Some(&bitstream_id_byte) = bytes.get(BITSTREAM_ID_BYTE_OFFSET) else {
        return Err(SyncframeHeaderError::Truncated {
            available_bytes: bytes.len(),
            required_bytes: BITSTREAM_ID_PROBE_BYTES,
        });
    };
    let bitstream_id = bitstream_id_byte >> 3;
    if bitstream_id <= MAX_AC3_BITSTREAM_ID {
        parse_ac3_header(bytes, bitstream_id)
    } else if EAC3_BITSTREAM_IDS.contains(&bitstream_id) {
        parse_eac3_header(bytes, bitstream_id)
    } else {
        Err(SyncframeHeaderError::UnsupportedBitstreamId { bitstream_id })
    }
}

/// AC-3 syncinfo + начало BSI (A/52 §5.3.1–5.3.2).
fn parse_ac3_header(
    bytes: &[u8],
    bitstream_id: u8,
) -> Result<SyncframeHeader, SyncframeHeaderError> {
    let mut reader = BitReader::new(bytes);
    reader.skip_bits(16)?; // syncword
    reader.skip_bits(16)?; // crc1
    let fscod = reader.read_u8(2)?;
    let frame_size_code = reader.read_u8(6)?;
    reader.skip_bits(5)?; // bsid (уже прочитан)
    reader.skip_bits(3)?; // bsmod
    let coding_mode = AudioCodingMode::from_acmod(reader.read_u8(3)?);
    if coding_mode.has_center_mix_level() {
        reader.skip_bits(2)?; // cmixlev
    }
    if coding_mode.has_surround_mix_level() {
        reader.skip_bits(2)?; // surmixlev
    }
    if coding_mode == AudioCodingMode::Stereo {
        reader.skip_bits(2)?; // dsurmod
    }
    let lfeon = reader.read_flag()?;

    // Длина кадра считается по таблице базовой частоты `fscod` (у bsid 9/10
    // байты кадра те же, вдвое/вчетверо ниже только частота и битрейт).
    let table_sample_rate_hz = sample_rate_from_fscod(fscod)?;
    let frame_length_bytes = ac3_frame_length_bytes(table_sample_rate_hz, frame_size_code)?;
    let sample_rate_hz = table_sample_rate_hz >> reduced_rate_shift(bitstream_id);
    finish_header(
        SyncframeHeader {
            layout: SyncframeLayout::Ac3,
            bitstream_id,
            frame_length_bytes,
            sample_rate_hz,
            samples_per_frame: AC3_AUDIO_BLOCKS_PER_SYNCFRAME * SAMPLES_PER_AUDIO_BLOCK,
            coding_mode,
            low_frequency_effects: LowFrequencyEffects::from_flag(lfeon),
        },
        reader.consumed_bytes(),
    )
}

/// Сдвиг частоты AC-3 по `bsid`: 0 до 8 включительно, 1 у bsid 9, 2 у bsid 10.
const fn reduced_rate_shift(bitstream_id: u8) -> u32 {
    bitstream_id.saturating_sub(FULL_RATE_AC3_BITSTREAM_ID) as u32
}

/// E-AC-3 syncinfo + BSI до `chanmap` включительно (A/52 §E.1.2.1–E.1.2.2).
fn parse_eac3_header(
    bytes: &[u8],
    bitstream_id: u8,
) -> Result<SyncframeHeader, SyncframeHeaderError> {
    let mut reader = BitReader::new(bytes);
    reader.skip_bits(16)?; // syncword
    let stream_type = match reader.read_u8(2)? {
        0 => Eac3StreamType::Independent,
        1 => Eac3StreamType::Dependent,
        2 => Eac3StreamType::Ac3Convertible,
        _ => return Err(SyncframeHeaderError::ReservedStreamType),
    };
    let substream_id = SubstreamId(reader.read_u8(3)?);
    let frmsiz = usize::from(reader.read_bits(11)?);
    let fscod = reader.read_u8(2)?;
    let (sample_rate_hz, audio_blocks) = if fscod == EAC3_REDUCED_RATE_FSCOD {
        let fscod2 = reader.read_u8(2)?;
        (
            reduced_sample_rate_from_fscod2(fscod2)?,
            AC3_AUDIO_BLOCKS_PER_SYNCFRAME,
        )
    } else {
        let numblkscod = reader.read_u8(2)?;
        (
            sample_rate_from_fscod(fscod)?,
            audio_blocks_from_numblkscod(numblkscod),
        )
    };
    let coding_mode = AudioCodingMode::from_acmod(reader.read_u8(3)?);
    let lfeon = reader.read_flag()?;
    reader.skip_bits(5)?; // bsid (уже прочитан)
    let channel_map = if stream_type == Eac3StreamType::Dependent {
        read_dependent_channel_map(&mut reader, coding_mode)?
    } else {
        DependentChannelMap::FromCodingMode
    };

    // frmsiz — длина кадра в 16-битных словах минус один (A/52 §E.1.3.1.3).
    let frame_length_bytes = (frmsiz + 1) * 2;
    finish_header(
        SyncframeHeader {
            layout: SyncframeLayout::EnhancedAc3(Eac3Substream {
                stream_type,
                substream_id,
                channel_map,
            }),
            bitstream_id,
            frame_length_bytes,
            sample_rate_hz,
            samples_per_frame: audio_blocks * SAMPLES_PER_AUDIO_BLOCK,
            coding_mode,
            low_frequency_effects: LowFrequencyEffects::from_flag(lfeon),
        },
        reader.consumed_bytes(),
    )
}

/// Пропускает dialnorm/compr и читает `chanmape`/`chanmap` dependent substream-а.
fn read_dependent_channel_map(
    reader: &mut BitReader<'_>,
    coding_mode: AudioCodingMode,
) -> Result<DependentChannelMap, SyncframeHeaderError> {
    skip_dialogue_normalization_and_compression(reader)?;
    if coding_mode == AudioCodingMode::DualMono {
        // У 1+1 второй канал несёт собственные dialnorm2/compr2.
        skip_dialogue_normalization_and_compression(reader)?;
    }
    if reader.read_flag()? {
        Ok(DependentChannelMap::Explicit(
            ChannelLocations::from_chanmap(reader.read_bits(16)?),
        ))
    } else {
        Ok(DependentChannelMap::FromCodingMode)
    }
}

fn skip_dialogue_normalization_and_compression(
    reader: &mut BitReader<'_>,
) -> Result<(), SyncframeHeaderError> {
    reader.skip_bits(5)?; // dialnorm
    if reader.read_flag()? {
        reader.skip_bits(8)?; // compr
    }
    Ok(())
}

fn finish_header(
    header: SyncframeHeader,
    header_bytes: usize,
) -> Result<SyncframeHeader, SyncframeHeaderError> {
    if header.frame_length_bytes < header_bytes {
        return Err(SyncframeHeaderError::FrameShorterThanHeader {
            frame_length_bytes: header.frame_length_bytes,
            header_bytes,
        });
    }
    Ok(header)
}
