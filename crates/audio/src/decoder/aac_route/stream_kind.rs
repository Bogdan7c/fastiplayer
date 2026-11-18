//! Что за AAC-поток — по его «паспорту» (`AudioSpecificConfig`, ISO/IEC 14496-3 §1.6.2.1).
//!
//! Модуль чистый: читает только заголовок ASC и не декодирует звук. От ответа
//! зависит маршрут (решения 347–349 плана моста):
//! - [`AacStreamKind::HighEfficiency`] — HE-AAC (SBR, может быть с PS) с ядром
//!   AAC-LC → декодер HE-AAC (FFmpeg);
//! - [`AacStreamKind::LowComplexity`] — обычный AAC-LC → Symphonia; если SBR
//!   найдётся внутри пакета, маршрут переключится на первом таком пакете;
//! - [`AacStreamKind::Other`] — всё остальное (Main, LTP, ELD, xHE-AAC/USAC и т. п.)
//!   → Symphonia, которая честно отказывает, как и раньше. xHE-AAC FFmpeg 8.1
//!   умеет, но владелец его не включал — поэтому маршрут решается здесь,
//!   а не по отказу Symphonia.
//!
//! Сигнализация HE-AAC бывает двух явных видов (сайты лент пишут первую):
//! - иерархическая: тип 5 (SBR) или 29 (PS), частота ядра, каналы, выходная
//!   частота, затем тип ядра (2 = AAC-LC);
//! - обратно совместимая: тип 2 (AAC-LC) + GASpecificConfig + расширение
//!   синхронизации `0x2B7` с типом 5 и флагом `sbrPresentFlag`, затем,
//!   возможно, `0x548` с флагом `psPresentFlag` (так пишут fdkaac и Apple).

/// Тип объекта AAC-LC.
const AUDIO_OBJECT_TYPE_LOW_COMPLEXITY: u32 = 2;
/// Тип объекта SBR (HE-AAC v1).
const AUDIO_OBJECT_TYPE_SBR: u32 = 5;
/// Тип объекта PS (HE-AAC v2, всегда вместе с SBR).
const AUDIO_OBJECT_TYPE_PARAMETRIC_STEREO: u32 = 29;
/// «Escape» типа объекта: дальше 6 бит, тип = 32 + значение.
const AUDIO_OBJECT_TYPE_ESCAPE: u32 = 31;
/// «Escape» индекса частоты: дальше 24 бита явной частоты.
const SAMPLING_FREQUENCY_INDEX_ESCAPE: u32 = 0xF;
/// Расширение синхронизации SBR в обратно совместимой сигнализации.
const SYNC_EXTENSION_SBR: u32 = 0x2B7;
/// Расширение синхронизации PS (после SBR).
const SYNC_EXTENSION_PARAMETRIC_STEREO: u32 = 0x548;

/// Как поток сообщил, что он HE-AAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HeAacSignalling {
    /// Тип 5/29 в начале ASC (Instagram, TikTok, большинство MP4-упаковщиков).
    Hierarchical,
    /// AAC-LC + расширение `0x2B7` в хвосте ASC (fdkaac, Apple).
    BackwardCompatible,
}

/// Есть ли в потоке Parametric Stereo (стерео из моно-ядра, HE-AAC v2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParametricStereo {
    Present,
    Absent,
}

/// Ответ классификатора.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AacStreamKind {
    /// HE-AAC с ядром AAC-LC.
    HighEfficiency {
        signalling: HeAacSignalling,
        parametric_stereo: ParametricStereo,
    },
    /// Обычный AAC-LC без признаков SBR в паспорте.
    LowComplexity,
    /// Другой тип объекта или паспорт, который мы не можем прочитать.
    Other,
}

/// Классифицирует поток по байтам `AudioSpecificConfig`.
pub(crate) fn classify_audio_specific_config(config_bytes: &[u8]) -> AacStreamKind {
    classify(&mut BitCursor::new(config_bytes)).unwrap_or(AacStreamKind::Other)
}

/// Чтение по полям ASC; `None` — паспорт короче, чем обещают его поля.
fn classify(cursor: &mut BitCursor<'_>) -> Option<AacStreamKind> {
    let object_type = read_audio_object_type(cursor)?;
    let _core_sampling_frequency = read_sampling_frequency(cursor)?;
    let channel_configuration = cursor.read(4)?;

    if matches!(
        object_type,
        AUDIO_OBJECT_TYPE_SBR | AUDIO_OBJECT_TYPE_PARAMETRIC_STEREO
    ) {
        let _output_sampling_frequency = read_sampling_frequency(cursor)?;
        let core_object_type = read_audio_object_type(cursor)?;
        if core_object_type != AUDIO_OBJECT_TYPE_LOW_COMPLEXITY {
            return Some(AacStreamKind::Other);
        }
        let parametric_stereo = if object_type == AUDIO_OBJECT_TYPE_PARAMETRIC_STEREO {
            ParametricStereo::Present
        } else {
            ParametricStereo::Absent
        };
        return Some(AacStreamKind::HighEfficiency {
            signalling: HeAacSignalling::Hierarchical,
            parametric_stereo,
        });
    }
    if object_type != AUDIO_OBJECT_TYPE_LOW_COMPLEXITY {
        return Some(AacStreamKind::Other);
    }
    Some(classify_low_complexity_tail(cursor, channel_configuration))
}

/// Хвост ASC обычного AAC-LC: GASpecificConfig и, возможно, расширение `0x2B7`.
fn classify_low_complexity_tail(
    cursor: &mut BitCursor<'_>,
    channel_configuration: u32,
) -> AacStreamKind {
    // channelConfiguration = 0: дальше Program Config Element переменной длины.
    // Его разбор — забота патча Symphonia; SBR такого потока найдёт проба
    // первого пакета (решение 349).
    if channel_configuration == 0 {
        return AacStreamKind::LowComplexity;
    }
    skip_ga_specific_config(cursor)
        .and_then(|()| read_backward_compatible_extension(cursor))
        .unwrap_or(AacStreamKind::LowComplexity)
}

/// GASpecificConfig для AAC-LC: frameLengthFlag, dependsOnCoreCoder (+14 бит), extensionFlag.
fn skip_ga_specific_config(cursor: &mut BitCursor<'_>) -> Option<()> {
    let _frame_length_flag = cursor.read(1)?;
    if cursor.read(1)? == 1 {
        let _core_coder_delay = cursor.read(14)?;
    }
    let _extension_flag = cursor.read(1)?;
    Some(())
}

/// Расширение `0x2B7` после GASpecificConfig (обратно совместимая сигнализация).
fn read_backward_compatible_extension(cursor: &mut BitCursor<'_>) -> Option<AacStreamKind> {
    if cursor.read(11)? != SYNC_EXTENSION_SBR {
        return None;
    }
    if read_audio_object_type(cursor)? != AUDIO_OBJECT_TYPE_SBR || cursor.read(1)? != 1 {
        // Расширение есть, но говорит «SBR нет» (так пишет ffmpeg для AAC-LC).
        return Some(AacStreamKind::LowComplexity);
    }
    let _output_sampling_frequency = read_sampling_frequency(cursor)?;
    let parametric_stereo = match (cursor.read(11), cursor.read(1)) {
        (Some(SYNC_EXTENSION_PARAMETRIC_STEREO), Some(1)) => ParametricStereo::Present,
        _ => ParametricStereo::Absent,
    };
    Some(AacStreamKind::HighEfficiency {
        signalling: HeAacSignalling::BackwardCompatible,
        parametric_stereo,
    })
}

fn read_audio_object_type(cursor: &mut BitCursor<'_>) -> Option<u32> {
    let object_type = cursor.read(5)?;
    if object_type == AUDIO_OBJECT_TYPE_ESCAPE {
        return Some(32 + cursor.read(6)?);
    }
    Some(object_type)
}

/// Индекс частоты (4 бита) или явная частота (24 бита после escape).
fn read_sampling_frequency(cursor: &mut BitCursor<'_>) -> Option<u32> {
    let index = cursor.read(4)?;
    if index == SAMPLING_FREQUENCY_INDEX_ESCAPE {
        return cursor.read(24);
    }
    Some(index)
}

/// Читатель битов старшим вперёд; за концом данных — `None`, без паники.
struct BitCursor<'a> {
    bytes: &'a [u8],
    bit_position: usize,
}

impl<'a> BitCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self {
            bytes,
            bit_position: 0,
        }
    }

    /// Читает `bit_count` (≤ 32) бит.
    fn read(&mut self, bit_count: u32) -> Option<u32> {
        let mut value = 0_u32;
        for _ in 0..bit_count {
            let byte = *self.bytes.get(self.bit_position / 8)?;
            let bit = (byte >> (7 - self.bit_position % 8)) & 1;
            value = (value << 1) | u32::from(bit);
            self.bit_position += 1;
        }
        Some(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HE_AAC_V1: AacStreamKind = AacStreamKind::HighEfficiency {
        signalling: HeAacSignalling::Hierarchical,
        parametric_stereo: ParametricStereo::Absent,
    };

    #[test]
    fn site_passports_are_hierarchical_he_aac() {
        // Значения сняты с Instagram и TikTok в разведке 16b1.
        assert_eq!(
            classify_audio_specific_config(&[0x2b, 0x92, 0x08, 0x00]),
            HE_AAC_V1
        );
        assert_eq!(
            classify_audio_specific_config(&[0x2b, 0x09, 0x88, 0x00]),
            HE_AAC_V1
        );
        assert_eq!(
            classify_audio_specific_config(&[0xeb, 0x8a, 0x08, 0x00]),
            AacStreamKind::HighEfficiency {
                signalling: HeAacSignalling::Hierarchical,
                parametric_stereo: ParametricStereo::Present,
            }
        );
    }

    #[test]
    fn fdkaac_passports_are_backward_compatible_he_aac() {
        assert_eq!(
            classify_audio_specific_config(&[0x13, 0x90, 0x56, 0xe5, 0xa0]),
            AacStreamKind::HighEfficiency {
                signalling: HeAacSignalling::BackwardCompatible,
                parametric_stereo: ParametricStereo::Absent,
            }
        );
        assert_eq!(
            classify_audio_specific_config(&[0x13, 0x88, 0x56, 0xe5, 0xa5, 0x48, 0x80]),
            AacStreamKind::HighEfficiency {
                signalling: HeAacSignalling::BackwardCompatible,
                parametric_stereo: ParametricStereo::Present,
            }
        );
    }

    #[test]
    fn plain_low_complexity_stays_on_symphonia() {
        // 44,1 кГц стерео, 48 кГц 5.1 — обычные паспорта AAC-LC.
        assert_eq!(
            classify_audio_specific_config(&[0x12, 0x10]),
            AacStreamKind::LowComplexity
        );
        assert_eq!(
            classify_audio_specific_config(&[0x11, 0xb0]),
            AacStreamKind::LowComplexity
        );
        // ffmpeg пишет хвост 0x2B7 с sbrPresentFlag = 0: это всё ещё AAC-LC.
        assert_eq!(
            classify_audio_specific_config(&[0x12, 0x10, 0x56, 0xe5, 0x00]),
            AacStreamKind::LowComplexity
        );
    }

    #[test]
    fn program_config_element_passport_stays_on_symphonia() {
        // AAC-LC 5.1 с PCE (channelConfiguration = 0), как пишет ffmpeg; хвост PCE не разбираем.
        let ffmpeg_pce_5p1: [u8; 25] = [
            0x11, 0x80, 0x04, 0xc8, 0x05, 0x00, 0x01, 0x08, 0x80, 0x0c, b'L', b'a', b'v', b'c',
            b'6', b'3', b'.', b'1', b'.', b'1', b'0', b'2', 0x56, 0xe5, 0x00,
        ];
        assert_eq!(
            classify_audio_specific_config(&ffmpeg_pce_5p1),
            AacStreamKind::LowComplexity
        );
    }

    #[test]
    fn xhe_aac_and_other_object_types_are_not_routed_to_ffmpeg() {
        // USAC (xHE-AAC, тип 42 = escape 31 + 10), AAC Main (1), LTP (4), ELD (39 = 31 + 7).
        assert_eq!(
            classify_audio_specific_config(&[0xf9, 0x46, 0x40]),
            AacStreamKind::Other
        );
        assert_eq!(
            classify_audio_specific_config(&[0x0a, 0x10]),
            AacStreamKind::Other
        );
        assert_eq!(
            classify_audio_specific_config(&[0x22, 0x10]),
            AacStreamKind::Other
        );
        assert_eq!(
            classify_audio_specific_config(&[0xf8, 0xe6, 0x10]),
            AacStreamKind::Other
        );
        // SBR поверх не-LC ядра (тип 5, ядро BSAC = 22) — тоже не наш маршрут.
        assert_eq!(
            classify_audio_specific_config(&[0x2b, 0x92, 0x58, 0x00]),
            AacStreamKind::Other
        );
    }

    #[test]
    fn truncated_or_empty_passport_is_other_without_panic() {
        assert_eq!(classify_audio_specific_config(&[]), AacStreamKind::Other);
        assert_eq!(
            classify_audio_specific_config(&[0x2b]),
            AacStreamKind::Other
        );
        for length in 0..4 {
            let prefix = &[0x2b, 0x92, 0x08, 0x00][..length];
            assert_ne!(
                classify_audio_specific_config(prefix),
                AacStreamKind::LowComplexity
            );
        }
    }

    #[test]
    fn explicit_frequency_escape_is_skipped_correctly() {
        // Тип 5, частота ядра явная (escape 0xF + 24 бита 22050), стерео, выход индекс 4, ядро LC.
        let mut bits = BitWriter::default();
        bits.write(5, 5);
        bits.write(0xF, 4);
        bits.write(22_050, 24);
        bits.write(2, 4);
        bits.write(4, 4);
        bits.write(2, 5);
        bits.write(0, 3);
        assert_eq!(
            classify_audio_specific_config(&bits.into_bytes()),
            HE_AAC_V1
        );
    }

    /// Писатель битов для собранных вручную паспортов.
    #[derive(Default)]
    struct BitWriter {
        bits: Vec<u8>,
    }

    impl BitWriter {
        fn write(&mut self, value: u32, count: u32) {
            for index in (0..count).rev() {
                self.bits.push(((value >> index) & 1) as u8);
            }
        }

        fn into_bytes(self) -> Vec<u8> {
            self.bits
                .chunks(8)
                .map(|chunk| {
                    chunk
                        .iter()
                        .enumerate()
                        .fold(0, |byte, (index, bit)| byte | (bit << (7 - index)))
                })
                .collect()
        }
    }
}
