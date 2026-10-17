//! Типизированные ошибки декодера.
//!
//! Правило crate-а: никакой вход не приводит к панике. Всё, что не является
//! корректным syncframe-ом, превращается в один из вариантов
//! [`Ac3DecodeError`]. Варианты разделены по смыслу для вызывающего кода:
//!
//! - `Truncated` — данных меньше, чем объявил заголовок (обрыв контейнера);
//! - `MissingSyncWord`, `Reserved*`, `UnsupportedBitstreamId` — это не AC-3
//!   кадр, который этот декодер умеет читать;
//! - `CrcMismatch` — кадр повреждён при передаче (A/52 §7.10.1);
//! - `BitstreamOverrun` / `InvalidBitstream` — CRC сошёлся (или проверка
//!   выключена), но синтаксис внутри кадра нарушен (A/52 §7.10.2);
//! - `Unsupported` — корректный поток, но возможность ещё не реализована.

use std::fmt;

/// Номер аудиоблока внутри syncframe-а AC-3 (0–5).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioBlockIndex(u8);

impl AudioBlockIndex {
    /// Создаёт индекс блока; вызывающий код гарантирует диапазон 0–5.
    pub(crate) const fn new(block: u8) -> Self {
        Self(block)
    }

    /// Номер блока (0 — первый блок кадра).
    #[must_use]
    pub const fn get(self) -> u8 {
        self.0
    }
}

/// Часть syncframe-а, при разборе которой обнаружена проблема.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrameSection {
    /// `syncinfo` + `bsi` (A/52 §5.3.1–5.3.2; для E-AC-3 — Annex E Таблицы E1.1–E1.2).
    BitStreamInformation,
    /// `audfrm` E-AC-3 — параметры всего кадра (Annex E Таблица E1.3).
    AudioFrame,
    /// Аудиоблок `audblk` с указанным номером (A/52 §5.3.3).
    AudioBlock(AudioBlockIndex),
    /// Хвост кадра: `auxdata` и `errorcheck` (A/52 §5.3.4–5.3.5).
    FrameTrailer,
}

/// Какая из двух CRC syncframe-а не сошлась (A/52 §7.10.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrcRegion {
    /// `crc1`: первые 5/8 кадра без синхрослова.
    FirstFiveEighths,
    /// `crc2`: последние 3/8 кадра.
    LastThreeEighths,
    /// Единственная CRC E-AC-3 (`crc2`), покрывающая весь кадр (Annex E §3.2).
    WholeFrame,
}

/// Элемент, который поток пытается «переиспользовать» из предыдущего блока,
/// хотя переиспользовать его нельзя.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReusedElement {
    /// Стратегия coupling (`cplstre == 0`), условие 1 §7.10.2.
    CouplingStrategy,
    /// Координаты coupling канала (`cplcoe == 0`), условия 4, 23, 24.
    CouplingCoordinates,
    /// Флаги rematrixing (`rematstr == 0`), условия 5, 25.
    RematrixFlags,
    /// Экспоненты coupling канала (`cplexpstr == reuse`), условия 6, 7.
    CouplingExponents,
    /// Экспоненты полнополосного канала (`chexpstr == reuse`), условия 8, 9.
    ChannelExponents,
    /// Экспоненты LFE (`lfeexpstr == reuse`), условие 10.
    LfeExponents,
    /// Параметрическая информация битового распределения (`baie == 0`), условие 12.
    BitAllocationParameters,
    /// SNR offset-ы (`snroffste == 0`), условие 13 и §5.5 для coupling.
    SnrOffsets,
    /// Leak-инициализация coupling канала (`cplleake == 0`), условие 14.
    CouplingLeak,
    /// Координаты спектрального расширения (`spxcoe == 0`) без прошлых
    /// координат под ту же разметку полос (E-AC-3, Annex E §2.3.3.9).
    SpectralExtensionCoordinates,
    /// Амплитуды улучшенного coupling (`ecplparam1e == 0`) без прошлых
    /// амплитуд под ту же разметку полос (E-AC-3, Annex E §2.3.3.21).
    EnhancedCouplingAmplitudes,
}

/// Нарушение синтаксиса или ограничений A/52 внутри кадра.
///
/// Номера в документации — номера условий из списка A/52 §7.10.2.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BitstreamViolation {
    /// Элемент объявлен «как в прошлом блоке», но прошлого корректного значения
    /// нет (первый блок, только что включённый coupling) или изменились
    /// размеры, к которым оно относилось.
    InvalidReuse(ReusedElement),
    /// Coupling включён в режиме 1+1 или 1/0 (условие 22).
    CouplingInSingleChannelMode,
    /// В coupling меньше двух каналов (условие 2).
    TooFewCoupledChannels {
        /// Сколько каналов отмечено `chincpl`.
        coupled_channels: u8,
    },
    /// `cplbegf > cplendf + 2` — пустой диапазон coupling (условие 3).
    InvertedCouplingRange {
        /// `cplbegf`.
        begin_subband: u8,
        /// `cplendf + 2` — последний sub-band.
        last_subband: u8,
    },
    /// `chbwcod > 60` (условие 11, A/52 §5.4.3.24).
    ChannelBandwidthCodeOutOfRange {
        /// Прочитанный `chbwcod`.
        code: u8,
    },
    /// Групповой код экспонент больше 124 (условие 17).
    GroupedExponentOutOfRange {
        /// Прочитанный 7-битный код.
        code: u8,
    },
    /// Декодированная экспонента вышла за 0..=24 (A/52 §7.1.1).
    ExponentOutOfRange,
    /// Зарезервированное состояние `cpldeltbae`/`deltbae` = 3 (Таблица 5.16).
    ReservedDeltaBitAllocationState,
    /// Сегмент delta bit allocation выходит за 50 полос (условия 15, 16).
    DeltaBitAllocationBeyondBands,
    /// Код мантиссы вне таблицы квантователя (условия 18–20; для bap 3 и 5 —
    /// неиспользуемые коды 7 и 15, см. Таблицы 7.21 и 7.23).
    MantissaCodeOutOfRange {
        /// Указатель битового распределения мантиссы.
        bap: u8,
        /// Прочитанный код.
        code: u16,
    },
    /// `strmtyp = 3` зарезервирован (Annex E Таблица E2.1).
    ReservedStreamType,
    /// `snroffststr = 3` зарезервирован (Annex E Таблица E2.9).
    ReservedSnrOffsetStrategy,
    /// Число позиций в `chanmap` не равно числу кодированных каналов
    /// зависимого substream-а (Annex E §2.3.1.8).
    ChannelMapMismatch {
        /// Сколько каналов описывает `chanmap` (пары — по два).
        mapped_channels: u8,
        /// `nfchans + lfeon` этого substream-а.
        coded_channels: u8,
    },
    /// Пустой диапазон спектрального расширения: `spx_begin_subbnd ≥
    /// spx_end_subbnd` (Annex E §2.3.3.5–2.3.3.6).
    InvertedSpectralExtensionRange {
        /// `spx_begin_subbnd`.
        begin_subband: u8,
        /// `spx_end_subbnd`.
        end_subband: u8,
    },
    /// Область копирования SPX пуста: `spxstrtf` не ниже начала расширения
    /// (Annex E §3.6.4.1: `copystartmant < copyendmant`).
    EmptySpectralExtensionCopyRange {
        /// `spxstrtf` — первый копируемый sub-band.
        copy_start_subband: u8,
        /// `spx_begin_subbnd`.
        begin_subband: u8,
    },
    /// Пустой диапазон улучшенного coupling: `ecpl_begin_subbnd ≥
    /// ecpl_end_subbnd` (Annex E §2.3.3.16–2.3.3.17).
    InvertedEnhancedCouplingRange {
        /// `ecpl_begin_subbnd`.
        begin_subband: u8,
        /// `ecpl_end_subbnd`.
        end_subband: u8,
    },
    /// Составной 5-битный код усилений GAQ больше 26 (три троичных значения,
    /// Annex E §3.4.4.2).
    GainAdaptiveCodeOutOfRange {
        /// Прочитанный код.
        code: u8,
    },
    /// `auxdatal` длиннее доступного `nauxbits` (A/52 §5.4.4.2).
    AuxiliaryDataTooLong {
        /// Заявленная длина пользовательских данных в битах.
        declared_bits: usize,
        /// Сколько бит реально осталось между аудио и хвостом.
        available_bits: usize,
    },
}

/// Нарушение состава access unit-а E-AC-3 (Annex E §2.3.1.2, §3.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccessUnitViolation {
    /// Access unit начинается с зависимого substream-а: независимого
    /// substream-а 0 (или ядра AC-3), к которому он относится, нет.
    DependentSubstreamWithoutIndependent,
    /// Access unit начинается с независимого substream-а с `substreamid ≠ 0`:
    /// substream 0 «shall be the first substream present» (§2.3.1.2).
    MissingIndependentSubstreamZero,
    /// Второй независимый substream 0 (или второе ядро AC-3) в одном access
    /// unit-е: это уже следующий кадр программы.
    RepeatedIndependentSubstream,
    /// Больше 8 зависимых substream-ов у одного независимого (§2.3.1.2).
    TooManyDependentSubstreams,
    /// Зависимый substream с другой частотой или другим числом блоков, чем
    /// у его независимого substream-а (§2.3.1.2: «shall»).
    DependentSubstreamFormatMismatch,
    /// Зависимый substream у потока, перекодированного из AC-3 (`strmtyp = 2`):
    /// «may not have any dependent streams» (§2.3.1.1).
    DependentSubstreamOfConvertedStream,
    /// Кадр AC-3 не в начале access unit-а: ядро AC-3 может быть только
    /// независимым substream-ом 0 (§2.3.1.2).
    MisplacedAc3Syncframe,
    /// После сведения substream-ов (§3.8.2) в программе больше 16 разных
    /// позиций каналов: «maximum number of channels rendered for a single
    /// program is 16».
    TooManyProgramChannels,
}

/// Распознанный вход, который этот путь API не разбирает.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnsupportedFeature {
    /// Syncframe E-AC-3 (`bsid` 11–16, A/52 Annex E) передан функции,
    /// которая разбирает только синтаксис AC-3 ([`crate::read_syncframe_info`]);
    /// для него есть [`crate::read_enhanced_syncframe_info`], а
    /// [`crate::Ac3Decoder`] декодирует оба синтаксиса.
    EnhancedAc3,
}

/// Ошибка декодирования одного access unit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ac3DecodeError {
    /// Байтов меньше, чем нужно заголовку или объявленной длине кадра.
    Truncated {
        /// Сколько байт было передано.
        available_bytes: usize,
        /// Сколько байт требуется.
        required_bytes: usize,
    },
    /// Первые два байта — не синхрослово `0x0B77` (A/52 §5.4.1.1).
    MissingSyncWord,
    /// `bsid` вне 0–10 (AC-3) и 11–16 (E-AC-3) (A/52 §5.4.2.1, Annex E).
    UnsupportedBitstreamId {
        /// Прочитанный `bsid`.
        bitstream_id: u8,
    },
    /// `fscod == 3` зарезервирован (Таблица 5.6).
    ReservedSampleRateCode,
    /// `frmsizecod` 38–63 зарезервированы (Таблица 5.18).
    ReservedFrameSizeCode {
        /// Прочитанный `frmsizecod`.
        frame_size_code: u8,
    },
    /// CRC кадра не сошлась (A/52 §7.10.1).
    CrcMismatch {
        /// Какая из двух CRC.
        region: CrcRegion,
    },
    /// Распаковка дошла до конца отведённой области кадра (A/52 §7.10.2,
    /// условие 21).
    BitstreamOverrun {
        /// Где именно.
        section: FrameSection,
    },
    /// Синтаксис кадра нарушен (A/52 §7.10.2).
    InvalidBitstream {
        /// Где именно.
        section: FrameSection,
        /// Что нарушено.
        violation: BitstreamViolation,
    },
    /// Состав access unit-а E-AC-3 нарушен (Annex E §3.8).
    InvalidAccessUnit(AccessUnitViolation),
    /// Корректный поток, но этот путь API его не разбирает.
    Unsupported(UnsupportedFeature),
}

impl Ac3DecodeError {
    /// Удобный конструктор нарушения внутри аудиоблока.
    pub(crate) const fn in_block(block: AudioBlockIndex, violation: BitstreamViolation) -> Self {
        Self::InvalidBitstream {
            section: FrameSection::AudioBlock(block),
            violation,
        }
    }
}

impl fmt::Display for FrameSection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BitStreamInformation => write!(formatter, "bit stream information"),
            Self::AudioFrame => write!(formatter, "audio frame"),
            Self::AudioBlock(block) => write!(formatter, "audio block {}", block.get()),
            Self::FrameTrailer => write!(formatter, "frame trailer"),
        }
    }
}

impl fmt::Display for Ac3DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidAccessUnit(violation) => {
                write!(formatter, "E-AC-3 access unit is invalid: {violation:?}")
            }
            Self::Truncated {
                available_bytes,
                required_bytes,
            } => write!(
                formatter,
                "AC-3 frame is truncated: {available_bytes} bytes available, {required_bytes} required"
            ),
            Self::MissingSyncWord => write!(formatter, "AC-3 sync word 0x0B77 is missing"),
            Self::UnsupportedBitstreamId { bitstream_id } => write!(
                formatter,
                "AC-3 bitstream id {bitstream_id} is reserved or unsupported"
            ),
            Self::ReservedSampleRateCode => write!(formatter, "AC-3 sample rate code is reserved"),
            Self::ReservedFrameSizeCode { frame_size_code } => write!(
                formatter,
                "AC-3 frame size code {frame_size_code} is reserved"
            ),
            Self::CrcMismatch { region } => match region {
                CrcRegion::FirstFiveEighths => {
                    write!(
                        formatter,
                        "AC-3 crc1 mismatch in the first 5/8 of the frame"
                    )
                }
                CrcRegion::LastThreeEighths => {
                    write!(formatter, "AC-3 crc2 mismatch in the last 3/8 of the frame")
                }
                CrcRegion::WholeFrame => write!(formatter, "E-AC-3 frame crc mismatch"),
            },
            Self::BitstreamOverrun { section } => {
                write!(formatter, "AC-3 {section} runs past its end of frame")
            }
            Self::InvalidBitstream { section, violation } => {
                write!(formatter, "AC-3 {section} is invalid: {violation:?}")
            }
            Self::Unsupported(feature) => match feature {
                UnsupportedFeature::EnhancedAc3 => write!(
                    formatter,
                    "E-AC-3 syncframe passed to an AC-3-only header parser"
                ),
            },
        }
    }
}

impl std::error::Error for Ac3DecodeError {}
