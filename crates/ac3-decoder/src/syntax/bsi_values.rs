//! Смысловые значения полей BSI (A/52 §5.4.2 и Annex D §2.3).
//!
//! Каждое перечисление соответствует таблице спецификации; зарезервированные
//! коды не теряются, а получают явный вариант `Reserved` и рекомендованное
//! спецификацией поведение (например, `cmixlev = '11'` → −4,5 дБ).

/// Режим кодирования каналов `acmod` (Таблица 5.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioCodingMode {
    /// `'000'` — 1+1: две независимые моно-программы Ch1, Ch2.
    DualMono,
    /// `'001'` — 1/0: C.
    Mono,
    /// `'010'` — 2/0: L, R.
    Stereo,
    /// `'011'` — 3/0: L, C, R.
    ThreeFront,
    /// `'100'` — 2/1: L, R, S.
    StereoMonoSurround,
    /// `'101'` — 3/1: L, C, R, S.
    ThreeFrontMonoSurround,
    /// `'110'` — 2/2: L, R, SL, SR.
    StereoStereoSurround,
    /// `'111'` — 3/2: L, C, R, SL, SR.
    ThreeFrontStereoSurround,
}

impl AudioCodingMode {
    /// Разбирает 3-битный `acmod` (все 8 значений определены).
    pub(crate) const fn from_code(acmod: u8) -> Self {
        match acmod & 0x7 {
            0 => Self::DualMono,
            1 => Self::Mono,
            2 => Self::Stereo,
            3 => Self::ThreeFront,
            4 => Self::StereoMonoSurround,
            5 => Self::ThreeFrontMonoSurround,
            6 => Self::StereoStereoSurround,
            _ => Self::ThreeFrontStereoSurround,
        }
    }

    /// Исходный код `acmod`.
    #[must_use]
    pub const fn code(self) -> u8 {
        match self {
            Self::DualMono => 0,
            Self::Mono => 1,
            Self::Stereo => 2,
            Self::ThreeFront => 3,
            Self::StereoMonoSurround => 4,
            Self::ThreeFrontMonoSurround => 5,
            Self::StereoStereoSurround => 6,
            Self::ThreeFrontStereoSurround => 7,
        }
    }

    /// Число полнополосных каналов `nfchans` (Таблица 5.8).
    #[must_use]
    pub const fn full_bandwidth_channels(self) -> usize {
        match self {
            Self::Mono => 1,
            Self::DualMono | Self::Stereo => 2,
            Self::ThreeFront | Self::StereoMonoSurround => 3,
            Self::ThreeFrontMonoSurround | Self::StereoStereoSurround => 4,
            Self::ThreeFrontStereoSurround => 5,
        }
    }

    /// Есть ли три фронтальных канала (поле `cmixlev`, Таблица 5.2).
    pub(crate) const fn has_three_front_channels(self) -> bool {
        let acmod = self.code();
        (acmod & 0x1) != 0 && acmod != 0x1
    }

    /// Есть ли surround-каналы (поле `surmixlev`, Таблица 5.2).
    pub(crate) const fn has_surround(self) -> bool {
        self.code() & 0x4 != 0
    }
}

/// Тип сервиса по `bsmod` и `acmod` (Таблица 5.7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ServiceType {
    /// `'000'` main: complete main (CM).
    CompleteMain,
    /// `'001'` main: music and effects (ME).
    MusicAndEffects,
    /// `'010'` associated: visually impaired (VI).
    VisuallyImpaired,
    /// `'011'` associated: hearing impaired (HI).
    HearingImpaired,
    /// `'100'` associated: dialogue (D).
    Dialogue,
    /// `'101'` associated: commentary (C).
    Commentary,
    /// `'110'` associated: emergency (E).
    Emergency,
    /// `'111'` при `acmod = '001'`: associated voice over (VO).
    VoiceOver,
    /// `'111'` при `acmod` 2–7: main karaoke.
    Karaoke,
}

impl ServiceType {
    /// Таблица 5.7. Код `'111'` при `acmod = 0` (1+1) таблицей не описан;
    /// считаем его караоке-программой, как для всех `acmod ≠ 1`.
    pub(crate) const fn from_codes(bsmod: u8, mode: AudioCodingMode) -> Self {
        match bsmod & 0x7 {
            0 => Self::CompleteMain,
            1 => Self::MusicAndEffects,
            2 => Self::VisuallyImpaired,
            3 => Self::HearingImpaired,
            4 => Self::Dialogue,
            5 => Self::Commentary,
            6 => Self::Emergency,
            _ => {
                if matches!(mode, AudioCodingMode::Mono) {
                    Self::VoiceOver
                } else {
                    Self::Karaoke
                }
            }
        }
    }
}

/// Уровень центра в downmix-е `cmixlev` (Таблица 5.9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CenterMixLevel {
    /// `'00'` — 0,707 (−3 дБ).
    Minus3Db,
    /// `'01'` — 0,595 (−4,5 дБ).
    Minus4p5Db,
    /// `'10'` — 0,500 (−6 дБ).
    Minus6Db,
    /// `'11'` — зарезервирован; §5.4.2.4 советует промежуточные −4,5 дБ.
    Reserved,
}

impl CenterMixLevel {
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::Minus3Db,
            1 => Self::Minus4p5Db,
            2 => Self::Minus6Db,
            _ => Self::Reserved,
        }
    }

    /// Коэффициент из Таблицы 5.9 (для `Reserved` — 0,595 по §5.4.2.4).
    #[must_use]
    pub const fn gain(self) -> f32 {
        match self {
            Self::Minus3Db => 0.707,
            Self::Minus4p5Db | Self::Reserved => 0.595,
            Self::Minus6Db => 0.500,
        }
    }
}

/// Уровень surround в downmix-е `surmixlev` (Таблица 5.10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SurroundMixLevel {
    /// `'00'` — 0,707 (−3 дБ).
    Minus3Db,
    /// `'01'` — 0,500 (−6 дБ).
    Minus6Db,
    /// `'10'` — 0 (surround не подмешивается).
    Muted,
    /// `'11'` — зарезервирован; §5.4.2.5 советует промежуточные −6 дБ.
    Reserved,
}

impl SurroundMixLevel {
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::Minus3Db,
            1 => Self::Minus6Db,
            2 => Self::Muted,
            _ => Self::Reserved,
        }
    }

    /// Коэффициент из Таблицы 5.10 (для `Reserved` — 0,5 по §5.4.2.5).
    #[must_use]
    pub const fn gain(self) -> f32 {
        match self {
            Self::Minus3Db => 0.707,
            Self::Minus6Db | Self::Reserved => 0.500,
            Self::Muted => 0.0,
        }
    }
}

/// Признак Dolby Surround `dsurmod` (Таблица 5.11), только в режиме 2/0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DolbySurroundMode {
    /// `'00'` — не указано.
    NotIndicated,
    /// `'01'` — не закодировано в Dolby Surround.
    NotEncoded,
    /// `'10'` — закодировано в Dolby Surround.
    Encoded,
    /// `'11'` — зарезервирован (§5.4.2.6: можно считать «не указано»).
    Reserved,
}

impl DolbySurroundMode {
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::NotIndicated,
            1 => Self::NotEncoded,
            2 => Self::Encoded,
            _ => Self::Reserved,
        }
    }
}

/// Тип комнаты сведения `roomtyp` (Таблица 5.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RoomType {
    /// `'00'` — не указано.
    NotIndicated,
    /// `'01'` — большая комната, мониторы с X-кривой.
    LargeRoomXCurve,
    /// `'10'` — маленькая комната, плоская АЧХ мониторов.
    SmallRoomFlat,
    /// `'11'` — зарезервирован.
    Reserved,
}

impl RoomType {
    pub(crate) const fn from_code(code: u8) -> Self {
        match code & 0x3 {
            0 => Self::NotIndicated,
            1 => Self::LargeRoomXCurve,
            2 => Self::SmallRoomFlat,
            _ => Self::Reserved,
        }
    }
}

/// Нормализация диалога `dialnorm` (§5.4.2.8).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DialogueNormalization {
    code: u8,
}

impl DialogueNormalization {
    pub(crate) const fn from_code(code: u8) -> Self {
        Self { code: code & 0x1F }
    }

    /// Исходный 5-битный код (0 — зарезервирован).
    #[must_use]
    pub const fn code(self) -> u8 {
        self.code
    }

    /// Уровень диалога относительно цифрового максимума, дБ (−1…−31).
    /// Зарезервированный код 0 по §5.4.2.8 трактуется как −31 дБ.
    #[must_use]
    pub const fn level_db(self) -> i8 {
        if self.code == 0 {
            -31
        } else {
            -(self.code as i8)
        }
    }
}

/// Сведения о студии сведения (`mixlevel` + `roomtyp`, §5.4.2.14–5.4.2.15).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioProductionInfo {
    /// Пиковый уровень сведения, дБ SPL: 80 + `mixlevel` (80…111).
    pub peak_mixing_level_db_spl: u8,
    /// Тип комнаты.
    pub room_type: RoomType,
}

/// Метаданные одной программы (основной или Ch2 в режиме 1+1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProgramMetadata {
    /// `dialnorm` / `dialnorm2`.
    pub dialogue_normalization: DialogueNormalization,
    /// Исходное 8-битное слово heavy compression `compr`/`compr2` (§7.7.2), если есть.
    pub compression_gain_word: Option<u8>,
    /// `langcod`/`langcod2` (§5.4.2.12: зарезервированное значение, обычно 0xFF).
    pub language_code: Option<u8>,
    /// `mixlevel`/`roomtyp` (или их Ch2-версии), если переданы.
    pub production: Option<AudioProductionInfo>,
}
