// fastiplayer patch: разбор Program Config Element (PCE) из AAC.
//
// ISO/IEC 14496-3, 4.4.1.1 (`program_config_element()`) и 1.6.2.1 (`AudioSpecificConfig`).
// PCE описывает раскладку каналов, когда `channel_configuration == 0`: так пишет, например,
// ffmpeg для 5.1(side) и других раскладок, у которых нет индекса 1..=7.
//
// Модуль владеет только разбором битов и сопоставлением списка элементов с одной из
// стандартных индексных конфигураций. Что делать с найденным индексом (какие destination
// planes использовать), решает `mod.rs`: он единственный владелец mapping-таблиц.

use std::borrow::Cow;

use symphonia_core::errors::{Result, decode_error, unsupported_error};
use symphonia_core::io::{BitReaderLtr, FiniteBitStream, ReadBitsLtr};

use super::AacChannelElementKind;

/// AOT AAC-LC в `AudioSpecificConfig` (единственный профиль, который умеет этот декодер).
const AUDIO_OBJECT_TYPE_AAC_LC: u32 = 2;
/// Значение audioObjectType, после которого тип дописан в 6 дополнительных бит.
const AUDIO_OBJECT_TYPE_ESCAPE: u32 = 31;
/// Значение samplingFrequencyIndex, после которого частота дописана в 24 бита.
const SAMPLING_FREQUENCY_INDEX_ESCAPE: u32 = 15;

/// Секция PCE, в которой объявлен element (определяет пространственную роль).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ProgramConfigSection {
    Front,
    Side,
    Back,
    LowFrequencyEffects,
}

/// Пространственная роль element-а в индексной конфигурации.
///
/// Side и Back намеренно слиты в `Surround`: индексные конфигурации 4..=7 различают только
/// «фронтальные», «окружающие» и LFE, а в mapping-таблицах `mod.rs` эти пары лежат в
/// одних и тех же surround-plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ElementRole {
    Front,
    Surround,
    LowFrequencyEffects,
}

impl ProgramConfigSection {
    const fn role(self) -> ElementRole {
        match self {
            Self::Front => ElementRole::Front,
            Self::Side | Self::Back => ElementRole::Surround,
            Self::LowFrequencyEffects => ElementRole::LowFrequencyEffects,
        }
    }
}

/// Один channel element, объявленный в PCE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ProgramConfigElement {
    pub(super) section: ProgramConfigSection,
    pub(super) kind: AacChannelElementKind,
    /// 4-bit element_instance_tag (на destination plane не влияет, см. `IndexedChannelElementCursor`).
    pub(super) instance_tag: u8,
}

/// Разобранный PCE: только то, что нужно для выбора раскладки.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProgramConfig {
    /// Elements в порядке объявления: front → side → back → lfe.
    pub(super) elements: Vec<ProgramConfigElement>,
    /// Количество coupling-channel элементов; декодер их не поддерживает.
    coupling_channel_count: u32,
}

/// Эталонные сигнатуры индексных конфигураций `channel_configuration` 1..=7.
///
/// Порядок элементов совпадает с порядком в `AAC_*_ELEMENTS` из `mod.rs`.
const INDEXED_CONFIGURATIONS: &[(u8, &[(ElementRole, AacChannelElementKind)])] = &[
    (1, &[(ElementRole::Front, AacChannelElementKind::Single)]),
    (2, &[(ElementRole::Front, AacChannelElementKind::Pair)]),
    (
        3,
        &[
            (ElementRole::Front, AacChannelElementKind::Single),
            (ElementRole::Front, AacChannelElementKind::Pair),
        ],
    ),
    (
        4,
        &[
            (ElementRole::Front, AacChannelElementKind::Single),
            (ElementRole::Front, AacChannelElementKind::Pair),
            (ElementRole::Surround, AacChannelElementKind::Single),
        ],
    ),
    (
        5,
        &[
            (ElementRole::Front, AacChannelElementKind::Single),
            (ElementRole::Front, AacChannelElementKind::Pair),
            (ElementRole::Surround, AacChannelElementKind::Pair),
        ],
    ),
    (
        6,
        &[
            (ElementRole::Front, AacChannelElementKind::Single),
            (ElementRole::Front, AacChannelElementKind::Pair),
            (ElementRole::Surround, AacChannelElementKind::Pair),
            (
                ElementRole::LowFrequencyEffects,
                AacChannelElementKind::LowFrequencyEffects,
            ),
        ],
    ),
    (
        7,
        &[
            (ElementRole::Front, AacChannelElementKind::Single),
            (ElementRole::Front, AacChannelElementKind::Pair),
            (ElementRole::Front, AacChannelElementKind::Pair),
            (ElementRole::Surround, AacChannelElementKind::Pair),
            (
                ElementRole::LowFrequencyEffects,
                AacChannelElementKind::LowFrequencyEffects,
            ),
        ],
    ),
];

impl ProgramConfig {
    /// Возвращает индекс стандартной `channel_configuration` (1..=7), эквивалентной этому PCE.
    ///
    /// Нестандартные раскладки и coupling channels — `Unsupported`, а не угадывание.
    pub(super) fn indexed_channel_configuration(&self) -> Result<u8> {
        if self.coupling_channel_count != 0 {
            return unsupported_error("aac: program config with coupling channel elements");
        }
        let actual_signature: Vec<(ElementRole, AacChannelElementKind)> = self
            .elements
            .iter()
            .map(|element| (element.section.role(), element.kind))
            .collect();

        match INDEXED_CONFIGURATIONS
            .iter()
            .find(|(_, expected_signature)| *expected_signature == actual_signature.as_slice())
        {
            Some((index, _)) => Ok(*index),
            None => unsupported_error("aac: program config layout has no indexed equivalent"),
        }
    }
}

/// Читает `program_config_element()` (без предшествующих 3 бит ID_PCE).
///
/// `alignment_origin_bits_left` — значение `bits_left()` в начале структуры, относительно
/// которой считается `byte_alignment()` (начало ASC или начало raw_data_block).
/// Все счётчики ограничены шириной своих полей, поэтому разбор ограничен по размеру,
/// а обрезанный вход даёт типизированную ошибку.
pub(super) fn read_program_config_element<B: ReadBitsLtr + FiniteBitStream>(
    bs: &mut B,
    alignment_origin_bits_left: u64,
) -> Result<ProgramConfig> {
    let _element_instance_tag = bs.read_bits_leq32(4)?;
    let _object_type = bs.read_bits_leq32(2)?;
    let _sampling_frequency_index = bs.read_bits_leq32(4)?;
    let front_count = bs.read_bits_leq32(4)?;
    let side_count = bs.read_bits_leq32(4)?;
    let back_count = bs.read_bits_leq32(4)?;
    let lfe_count = bs.read_bits_leq32(2)?;
    let assoc_data_count = bs.read_bits_leq32(3)?;
    let coupling_channel_count = bs.read_bits_leq32(4)?;

    // Mixdown-метаданные не влияют на декодирование: читаем только чтобы продвинуть поток.
    if bs.read_bool()? {
        let _mono_mixdown_element_number = bs.read_bits_leq32(4)?;
    }
    if bs.read_bool()? {
        let _stereo_mixdown_element_number = bs.read_bits_leq32(4)?;
    }
    if bs.read_bool()? {
        let _matrix_mixdown_idx_and_pseudo_surround = bs.read_bits_leq32(3)?;
    }

    let mut elements = Vec::new();
    for (section, count) in [
        (ProgramConfigSection::Front, front_count),
        (ProgramConfigSection::Side, side_count),
        (ProgramConfigSection::Back, back_count),
    ] {
        for _ in 0..count {
            let kind = if bs.read_bool()? {
                AacChannelElementKind::Pair
            } else {
                AacChannelElementKind::Single
            };
            let instance_tag = bs.read_bits_leq32(4)? as u8;
            elements.push(ProgramConfigElement {
                section,
                kind,
                instance_tag,
            });
        }
    }
    for _ in 0..lfe_count {
        let instance_tag = bs.read_bits_leq32(4)? as u8;
        elements.push(ProgramConfigElement {
            section: ProgramConfigSection::LowFrequencyEffects,
            kind: AacChannelElementKind::LowFrequencyEffects,
            instance_tag,
        });
    }
    for _ in 0..assoc_data_count {
        let _associated_data_element_tag = bs.read_bits_leq32(4)?;
    }
    for _ in 0..coupling_channel_count {
        let _is_independently_switched = bs.read_bool()?;
        let _coupling_element_tag = bs.read_bits_leq32(4)?;
    }

    // byte_alignment() считается от начала структуры-владельца, а не от начала PCE.
    let consumed_bits = alignment_origin_bits_left.saturating_sub(bs.bits_left());
    let padding_bits = (8 - consumed_bits % 8) % 8;
    bs.ignore_bits(padding_bits as u32)?;

    let comment_bytes = bs.read_bits_leq32(8)?;
    bs.ignore_bits(comment_bytes * 8)?;

    Ok(ProgramConfig {
        elements,
        coupling_channel_count,
    })
}

/// Приводит `AudioSpecificConfig` с `channel_configuration == 0` к индексной форме.
///
/// Если `channel_configuration != 0`, буфер возвращается без изменений (прежнее поведение).
/// Иначе разбирается PCE и в копии ASC поле channelConfiguration заменяется на эквивалентный
/// индекс 1..=7. Это чище, чем собирать `AudioSpecificConfig` вручную: upstream-разбор
/// sample rate/SBR остаётся единственным, а PCE-хвост после GASpecificConfig он не читает.
pub(super) fn resolve_indexed_audio_specific_config(extra_data: &[u8]) -> Result<Cow<'_, [u8]>> {
    let total_bits = 8 * extra_data.len() as u64;
    let mut bs = BitReaderLtr::new(extra_data);

    let mut object_type = bs.read_bits_leq32(5)?;
    if object_type == AUDIO_OBJECT_TYPE_ESCAPE {
        object_type = 32 + bs.read_bits_leq32(6)?;
    }
    if bs.read_bits_leq32(4)? == SAMPLING_FREQUENCY_INDEX_ESCAPE {
        let _explicit_sampling_frequency = bs.read_bits_leq32(24)?;
    }

    let channel_config_bit_offset = total_bits - bs.bits_left();
    if bs.read_bits_leq32(4)? != 0 {
        return Ok(Cow::Borrowed(extra_data));
    }

    // Дальше разбирается GASpecificConfig, а PCE допустим только для AAC-LC.
    if object_type != AUDIO_OBJECT_TYPE_AAC_LC {
        return unsupported_error("aac: program config is supported only for AAC-LC");
    }
    let _frame_length_flag = bs.read_bool()?;
    if bs.read_bool()? {
        let _core_coder_delay = bs.read_bits_leq32(14)?;
    }
    let _extension_flag = bs.read_bool()?;

    let program_config = read_program_config_element(&mut bs, total_bits)?;
    let index = program_config.indexed_channel_configuration()?;

    let mut rewritten = extra_data.to_vec();
    write_channel_configuration(&mut rewritten, channel_config_bit_offset, index)?;
    Ok(Cow::Owned(rewritten))
}

/// Записывает 4-bit channelConfiguration в буфер начиная с произвольного битового смещения.
fn write_channel_configuration(buf: &mut [u8], bit_offset: u64, index: u8) -> Result<()> {
    for bit_in_field in 0..4_u64 {
        let position = bit_offset + bit_in_field;
        let Some(byte) = buf.get_mut((position / 8) as usize) else {
            return decode_error("aac: channel configuration field is out of bounds");
        };
        let mask = 0x80_u8 >> (position % 8);
        if (index >> (3 - bit_in_field)) & 1 == 1 {
            *byte |= mask;
        } else {
            *byte &= !mask;
        }
    }
    Ok(())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    /// Реальный extradata ffmpeg для AAC-LC 48 кГц 5.1(side): channelConfiguration 0 + PCE
    /// (front SCE, front CPE, back CPE, LFE) + комментарий "Lavc63.1.102" + SBR-sync-хвост.
    pub(crate) const FFMPEG_PCE_5P1_EXTRA_DATA: [u8; 25] = [
        0x11, 0x80, 0x04, 0xc8, 0x05, 0x00, 0x01, 0x08, 0x80, 0x0c, b'L', b'a', b'v', b'c', b'6',
        b'3', b'.', b'1', b'.', b'1', b'0', b'2', 0x56, 0xe5, 0x00,
    ];

    /// Минимальный MSB-first писатель битов для синтетических PCE в тестах.
    #[derive(Default)]
    pub(crate) struct BitWriter {
        bits: Vec<bool>,
    }

    impl BitWriter {
        pub(crate) fn push(&mut self, value: u32, width: u32) -> &mut Self {
            for shift in (0..width).rev() {
                self.bits.push((value >> shift) & 1 == 1);
            }
            self
        }

        pub(crate) fn into_bytes(self) -> Vec<u8> {
            self.bits
                .chunks(8)
                .map(|chunk| {
                    chunk
                        .iter()
                        .enumerate()
                        .fold(0_u8, |byte, (i, bit)| byte | (u8::from(*bit) << (7 - i)))
                })
                .collect()
        }
    }

    /// Строит PCE-тело (без ID_PCE) для списка front/side/back/lfe elements, начиная с бита
    /// `leading_bits` внутри структуры-владельца; `elements` — (секция 0..=2, is_cpe).
    pub(crate) fn pce_bits(
        writer: &mut BitWriter,
        front: &[bool],
        side: &[bool],
        back: &[bool],
        lfe: u32,
        coupling: u32,
        origin_bit_count: usize,
    ) {
        writer.push(0, 4).push(1, 2).push(3, 4);
        writer
            .push(front.len() as u32, 4)
            .push(side.len() as u32, 4)
            .push(back.len() as u32, 4);
        writer.push(lfe, 2).push(0, 3).push(coupling, 4);
        writer.push(0, 3); // без mixdown-полей
        for (tag, is_cpe) in front.iter().chain(side).chain(back).enumerate() {
            writer.push(u32::from(*is_cpe), 1).push(tag as u32, 4);
        }
        for tag in 0..lfe {
            writer.push(tag, 4);
        }
        for tag in 0..coupling {
            writer.push(0, 1).push(tag, 4);
        }
        while (origin_bit_count + writer.bits.len()) % 8 != 0 {
            writer.push(0, 1);
        }
        writer.push(0, 8); // пустой comment
    }

    /// ASC AAC-LC 48 кГц, channelConfiguration 0 и PCE из заданных списков.
    fn asc_with_pce(
        front: &[bool],
        side: &[bool],
        back: &[bool],
        lfe: u32,
        coupling: u32,
    ) -> Vec<u8> {
        let mut writer = BitWriter::default();
        writer.push(2, 5).push(3, 4).push(0, 4).push(0, 3);
        let origin = 0;
        pce_bits(&mut writer, front, side, back, lfe, coupling, origin);
        writer.into_bytes()
    }

    #[test]
    fn ffmpeg_pce_extra_data_resolves_to_indexed_5p1() {
        let resolved = resolve_indexed_audio_specific_config(&FFMPEG_PCE_5P1_EXTRA_DATA).unwrap();

        // Тот же ASC, который ffmpeg пишет для обычного 5.1: 11 b0 (channelConfiguration = 6).
        assert_eq!(&resolved[..2], &[0x11, 0xb0]);
        assert_eq!(&resolved[2..], &FFMPEG_PCE_5P1_EXTRA_DATA[2..]);
    }

    #[test]
    fn indexed_asc_is_returned_unchanged() {
        let indexed = [0x11, 0xb0];
        let resolved = resolve_indexed_audio_specific_config(&indexed).unwrap();

        assert!(matches!(resolved, Cow::Borrowed(_)));
        assert_eq!(&*resolved, &indexed);
    }

    #[test]
    fn truncated_pce_is_a_decode_error_for_every_prefix() {
        // До конца комментария (22 байта) любая обрезка обязана быть ошибкой, а не паникой.
        for length in 2..22 {
            let result =
                resolve_indexed_audio_specific_config(&FFMPEG_PCE_5P1_EXTRA_DATA[..length]);
            assert!(result.is_err(), "prefix of {length} bytes must be rejected");
        }
        assert!(resolve_indexed_audio_specific_config(&FFMPEG_PCE_5P1_EXTRA_DATA[..22]).is_ok());
    }

    #[test]
    fn standard_layouts_resolve_to_their_indexes() {
        let (sce, cpe) = (false, true);
        // (front, side, back, lfe_count, ожидаемый индекс channelConfiguration)
        type LayoutCase<'a> = (&'a [bool], &'a [bool], &'a [bool], u32, u8);
        let cases: [LayoutCase<'_>; 6] = [
            (&[sce], &[], &[], 0, 1),
            (&[cpe], &[], &[], 0, 2),
            (&[sce, cpe], &[], &[sce], 0, 4),
            (&[sce, cpe], &[cpe], &[], 0, 5),
            (&[sce, cpe], &[], &[cpe], 1, 6),
            (&[sce, cpe, cpe], &[], &[cpe], 1, 7),
        ];
        for (front, side, back, lfe, expected_index) in cases {
            let asc = asc_with_pce(front, side, back, lfe, 0);
            let resolved = resolve_indexed_audio_specific_config(&asc).unwrap();
            assert_eq!(
                resolved[1] >> 3 & 0x0f,
                expected_index,
                "front={front:?} back={back:?}"
            );
        }
    }

    #[test]
    fn unknown_layout_and_coupling_are_unsupported() {
        let (sce, cpe) = (false, true);
        // Квадро (front CPE + back CPE) индексного эквивалента не имеет.
        let quad = asc_with_pce(&[cpe], &[], &[cpe], 0, 0);
        let coupled = asc_with_pce(&[sce, cpe], &[], &[cpe], 1, 1);

        for asc in [quad, coupled] {
            let error = resolve_indexed_audio_specific_config(&asc).unwrap_err();
            assert!(
                matches!(error, symphonia_core::errors::Error::Unsupported(_)),
                "{error:?}"
            );
        }
    }

    #[test]
    fn pce_is_rejected_for_non_lc_object_types() {
        // AOT 1 (Main) с channelConfiguration 0.
        let mut asc = asc_with_pce(&[false], &[], &[], 0, 0);
        asc[0] = (asc[0] & 0b0000_0111) | (1 << 3);
        let error = resolve_indexed_audio_specific_config(&asc).unwrap_err();
        assert!(matches!(
            error,
            symphonia_core::errors::Error::Unsupported(_)
        ));
    }
}
