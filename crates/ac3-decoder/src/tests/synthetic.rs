//! Синтетические кадры, собранные побитно по Таблицам 5.1–5.5.
//!
//! ffmpeg не умеет выдавать некоторые конструкции (1+1, `addbsi`, тайм-код,
//! `bsid` 9/10) и, конечно, не выдаёт нарушений §7.10.2. Здесь кадры собираются
//! вручную: минимальный корректный кадр + точечные отклонения. CRC у таких
//! кадров не считается — они декодируются с `CrcPolicy::Skip`.

use super::support::decoder;
use crate::config::CrcPolicy;
use crate::error::{
    Ac3DecodeError, AudioBlockIndex, BitstreamViolation, FrameSection, ReusedElement,
};
use crate::read_syncframe_info;
use crate::spectral::dither::DitherMode;
use crate::syntax::annex_d::BsiExtension;
use crate::syntax::bsi_values::{AudioCodingMode, RoomType};

/// Побитовый писатель MSB-first.
#[derive(Default)]
struct BitWriter {
    bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    fn put(&mut self, value: u32, width: u32) {
        for bit in (0..width).rev() {
            if self.bits.is_multiple_of(8) {
                self.bytes.push(0);
            }
            if (value >> bit) & 1 == 1 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }

    fn flag(&mut self, value: bool) {
        self.put(u32::from(value), 1);
    }
}

/// `frmsizecod` 16 при 48 кГц: 256 слов = 512 байт — с запасом для любых блоков.
const FRAME_SIZE_CODE: u32 = 16;
const FRAME_BYTES: usize = 512;

/// Сегмент delta bit allocation: `deltoffst`, `deltlen`, `deltba`.
type DeltaSegmentSpec = (u8, u8, u8);

/// Изменение блока + ожидаемый элемент, reuse которого запрещён.
type ReuseCase = (fn(&mut BlockSpec), ReusedElement);

/// Отклонения блока от минимального корректного варианта.
#[derive(Clone)]
struct BlockSpec {
    /// `cplstre`/`cplinu`/`chincpl`/`cplbegf`/`cplendf`; `None` → `cplstre = 0`.
    coupling: Option<CouplingSpec>,
    /// Координаты для coupled каналов (`cplcoe = 1`) или reuse (`false`).
    send_coordinates: bool,
    /// `rematstr` (только 2/0).
    rematrix_strategy: bool,
    /// `chexpstr` всех fbw каналов (0 — reuse).
    exponent_strategy: u8,
    /// `chbwcod` для каналов вне coupling.
    bandwidth_code: u8,
    /// 7-битные групповые коды (повторяются на нужное число групп).
    exponent_group_code: u8,
    /// `exps[ch][0]`.
    absolute_exponent: u8,
    baie: bool,
    snr_offsets: Option<(u8, u8)>,
    /// `deltbaie` + код и сегменты первого канала.
    delta: Option<(u8, Vec<DeltaSegmentSpec>)>,
    /// `skipl` в байтах.
    skip_bytes: Option<u16>,
}

#[derive(Clone, Copy)]
struct CouplingSpec {
    in_use: bool,
    in_coupling: [bool; 2],
    begin: u8,
    end: u8,
}

impl BlockSpec {
    /// Блок 0: всё передаётся, SNR offset-ы нулевые → все bap = 0, мантисс нет.
    fn first() -> Self {
        Self {
            coupling: Some(CouplingSpec {
                in_use: false,
                in_coupling: [false; 2],
                begin: 0,
                end: 0,
            }),
            send_coordinates: true,
            rematrix_strategy: true,
            exponent_strategy: 3,
            bandwidth_code: 0,
            exponent_group_code: 62, // 2,2,2 → разности 0: экспоненты не меняются
            absolute_exponent: 12,
            baie: true,
            snr_offsets: Some((0, 0)),
            delta: None,
            skip_bytes: None,
        }
    }

    /// Блоки 1–5: всё переиспользуется.
    fn reuse() -> Self {
        Self {
            coupling: None,
            send_coordinates: false,
            rematrix_strategy: false,
            exponent_strategy: 0,
            baie: false,
            snr_offsets: None,
            ..Self::first()
        }
    }
}

/// Пишет BSI минимального кадра (bsid 8, без опциональных полей).
fn write_minimal_bsi(writer: &mut BitWriter, acmod: u32) {
    writer.put(8, 5); // bsid
    writer.put(0, 3); // bsmod
    writer.put(acmod, 3);
    if (acmod & 1) != 0 && acmod != 1 {
        writer.put(1, 2); // cmixlev
    }
    if acmod & 4 != 0 {
        writer.put(1, 2); // surmixlev
    }
    if acmod == 2 {
        writer.put(0, 2); // dsurmod
    }
    writer.flag(false); // lfeon
    for _ in 0..if acmod == 0 { 2 } else { 1 } {
        writer.put(31, 5); // dialnorm
        writer.put(0, 3); // compre, langcode, audprodie
    }
    writer.put(0b01, 2); // copyrightb = 0, origbs = 1
    writer.put(0, 3); // timecod1e, timecod2e, addbsie
}

fn write_block(writer: &mut BitWriter, acmod: u32, channels: usize, spec: &BlockSpec) {
    writer.put(0, (2 * channels) as u32); // blksw, dithflag
    writer.flag(false); // dynrnge
    if acmod == 0 {
        writer.flag(false); // dynrng2e
    }
    let coupling_in_use = match spec.coupling {
        Some(coupling) => {
            writer.flag(true);
            writer.flag(coupling.in_use);
            if coupling.in_use {
                for &flag in coupling.in_coupling.iter().take(channels) {
                    writer.flag(flag);
                }
                if acmod == 2 {
                    writer.flag(false); // phsflginu
                }
                writer.put(u32::from(coupling.begin), 4);
                writer.put(u32::from(coupling.end), 4);
                let subbands = 3 + i32::from(coupling.end) - i32::from(coupling.begin);
                for _ in 1..subbands.max(0) {
                    writer.flag(false); // cplbndstrc
                }
            }
            coupling.in_use.then_some(coupling)
        }
        None => {
            writer.flag(false);
            None
        }
    };
    if let Some(coupling) = coupling_in_use {
        let bands = (3 + u32::from(coupling.end)).saturating_sub(u32::from(coupling.begin));
        for &flag in coupling.in_coupling.iter().take(channels) {
            if flag {
                writer.flag(spec.send_coordinates);
                if spec.send_coordinates {
                    writer.put(0, 2); // mstrcplco
                    writer.put(0, 8 * bands); // cplcoexp/cplcomant
                }
            }
        }
    }
    if acmod == 2 {
        writer.flag(spec.rematrix_strategy);
        if spec.rematrix_strategy {
            writer.put(0, 4);
        }
    }
    if coupling_in_use.is_some() {
        writer.put(0, 2); // cplexpstr = reuse (для тестов нарушений достаточно)
    }
    for _ in 0..channels {
        writer.put(u32::from(spec.exponent_strategy), 2);
    }
    if spec.exponent_strategy != 0 {
        for _ in 0..channels {
            writer.put(u32::from(spec.bandwidth_code), 6);
        }
        let end = (usize::from(spec.bandwidth_code) + 12) * 3 + 37;
        let bins_per_group = [0, 3, 6, 12][usize::from(spec.exponent_strategy)];
        let groups = (end - 1 + bins_per_group - 3) / bins_per_group;
        for _ in 0..channels {
            writer.put(u32::from(spec.absolute_exponent), 4);
            for _ in 0..groups {
                writer.put(u32::from(spec.exponent_group_code), 7);
            }
            writer.put(0, 2); // gainrng
        }
    }
    writer.flag(spec.baie);
    if spec.baie {
        writer.put(0b100_1011_0111, 11);
    }
    writer.flag(spec.snr_offsets.is_some());
    if let Some((coarse, fine)) = spec.snr_offsets {
        writer.put(u32::from(coarse), 6);
        for _ in 0..channels {
            writer.put(u32::from(fine), 4);
            writer.put(4, 3);
        }
    }
    writer.flag(spec.delta.is_some());
    if let Some((code, segments)) = &spec.delta {
        writer.put(u32::from(*code), 2);
        for _ in 1..channels {
            writer.put(2, 2); // остальные каналы — «без поправок»
        }
        if *code == 1 {
            writer.put((segments.len() - 1) as u32, 3);
            for &(offset, length, adjustment) in segments {
                writer.put(u32::from(offset), 5);
                writer.put(u32::from(length), 4);
                writer.put(u32::from(adjustment), 3);
            }
        }
    }
    writer.flag(spec.skip_bytes.is_some());
    if let Some(skip) = spec.skip_bytes {
        writer.put(u32::from(skip), 9);
        for _ in 0..skip {
            writer.put(0, 8);
        }
    }
}

/// Собирает кадр 48 кГц с заданными блоками; хвост: `auxdatae`, `crcrsv`, `crc2 = 0`.
fn build_frame(acmod: u32, blocks: &[BlockSpec], aux_user_bits: Option<u16>) -> Vec<u8> {
    let channels = AudioCodingMode::from_code(acmod as u8).full_bandwidth_channels();
    let mut writer = BitWriter::default();
    writer.put(0x0B77, 16);
    writer.put(0, 16); // crc1
    writer.put(0, 2); // fscod 48 кГц
    writer.put(FRAME_SIZE_CODE, 6);
    write_minimal_bsi(&mut writer, acmod);
    for spec in blocks {
        write_block(&mut writer, acmod, channels, spec);
    }
    let trailer_bits = 18 + if aux_user_bits.is_some() { 14 } else { 0 };
    let padding = (FRAME_BYTES * 8).saturating_sub(writer.bits + trailer_bits);
    for _ in 0..padding {
        writer.flag(false);
    }
    if let Some(length) = aux_user_bits {
        writer.put(u32::from(length), 14);
    }
    writer.flag(aux_user_bits.is_some());
    writer.put(0, 17);
    writer.bytes.truncate(FRAME_BYTES);
    writer.bytes
}

fn valid_blocks() -> Vec<BlockSpec> {
    let mut blocks = vec![BlockSpec::first()];
    blocks.extend(std::iter::repeat_n(BlockSpec::reuse(), 5));
    blocks
}

fn decode(frame: &[u8]) -> Result<crate::syntax::syncframe::DecodedSyncframe, Ac3DecodeError> {
    decoder(CrcPolicy::Skip, DitherMode::Disabled).decode_coefficients(frame)
}

fn block_violation(block: u8, violation: BitstreamViolation) -> Ac3DecodeError {
    Ac3DecodeError::InvalidBitstream {
        section: FrameSection::AudioBlock(AudioBlockIndex::new(block)),
        violation,
    }
}

#[test]
fn minimal_frames_decode_and_account_every_bit() {
    for acmod in [1, 2, 3, 6, 7] {
        let decoded = decode(&build_frame(acmod, &valid_blocks(), None)).unwrap();
        // Все bap = 0, мантисс нет: аудио кончается там, где кончились поля блоков,
        // а nauxbits = кадр − хвост − аудио.
        assert_eq!(
            decoded.trailer.auxiliary_bits + decoded.trailer.audio_end_bits + 18,
            FRAME_BYTES * 8,
            "acmod {acmod}"
        );
    }
}

#[test]
fn reuse_in_first_block_is_rejected_per_section_7_10_2() {
    let cases: [ReuseCase; 4] = [
        (|spec| spec.coupling = None, ReusedElement::CouplingStrategy),
        (
            |spec| spec.exponent_strategy = 0,
            ReusedElement::ChannelExponents,
        ),
        (
            |spec| spec.baie = false,
            ReusedElement::BitAllocationParameters,
        ),
        (|spec| spec.snr_offsets = None, ReusedElement::SnrOffsets),
    ];
    for (mutate, element) in cases {
        let mut blocks = valid_blocks();
        mutate(&mut blocks[0]);
        assert_eq!(
            decode(&build_frame(2, &blocks, None)),
            Err(block_violation(
                0,
                BitstreamViolation::InvalidReuse(element)
            )),
            "{element:?}"
        );
    }
    let mut blocks = valid_blocks();
    blocks[0].rematrix_strategy = false;
    assert_eq!(
        decode(&build_frame(2, &blocks, None)),
        Err(block_violation(
            0,
            BitstreamViolation::InvalidReuse(ReusedElement::RematrixFlags)
        ))
    );
}

#[test]
fn coupling_constraints_are_enforced() {
    let coupled = |in_coupling, begin, end| CouplingSpec {
        in_use: true,
        in_coupling,
        begin,
        end,
    };
    let mut mono = valid_blocks();
    mono[0].coupling = Some(coupled([true, true], 0, 0));
    assert_eq!(
        decode(&build_frame(1, &mono, None)),
        Err(block_violation(
            0,
            BitstreamViolation::CouplingInSingleChannelMode
        ))
    );
    let mut lonely = valid_blocks();
    lonely[0].coupling = Some(coupled([true, false], 0, 0));
    assert_eq!(
        decode(&build_frame(2, &lonely, None)),
        Err(block_violation(
            0,
            BitstreamViolation::TooFewCoupledChannels {
                coupled_channels: 1
            }
        ))
    );
    let mut inverted = valid_blocks();
    inverted[0].coupling = Some(coupled([true, true], 9, 2));
    assert_eq!(
        decode(&build_frame(2, &inverted, None)),
        Err(block_violation(
            0,
            BitstreamViolation::InvertedCouplingRange {
                begin_subband: 9,
                last_subband: 4
            }
        ))
    );
    // Coupling включён в блоке 2, координаты «как раньше» — переиспользовать нечего.
    let mut late = valid_blocks();
    late[2].coupling = Some(coupled([true, true], 0, 0));
    late[2].send_coordinates = false;
    assert_eq!(
        decode(&build_frame(2, &late, None)),
        Err(block_violation(
            2,
            BitstreamViolation::InvalidReuse(ReusedElement::CouplingCoordinates)
        ))
    );
}

#[test]
fn invalid_exponent_codes_are_rejected() {
    let mut out_of_table = valid_blocks();
    out_of_table[0].exponent_group_code = 125;
    assert_eq!(
        decode(&build_frame(1, &out_of_table, None)),
        Err(block_violation(
            0,
            BitstreamViolation::GroupedExponentOutOfRange { code: 125 }
        ))
    );
    let mut wide = valid_blocks();
    wide[0].bandwidth_code = 61;
    assert_eq!(
        decode(&build_frame(1, &wide, None)),
        Err(block_violation(
            0,
            BitstreamViolation::ChannelBandwidthCodeOutOfRange { code: 61 }
        ))
    );
    // Код 124 = +2,+2,+2: от 12 за 24 бина экспонента уходит за 24.
    let mut climbing = valid_blocks();
    climbing[0].exponent_group_code = 124;
    assert_eq!(
        decode(&build_frame(1, &climbing, None)),
        Err(block_violation(0, BitstreamViolation::ExponentOutOfRange))
    );
}

#[test]
fn delta_bit_allocation_errors_are_typed() {
    let mut reserved = valid_blocks();
    reserved[0].delta = Some((3, Vec::new()));
    assert_eq!(
        decode(&build_frame(1, &reserved, None)),
        Err(block_violation(
            0,
            BitstreamViolation::ReservedDeltaBitAllocationState
        ))
    );
    // Сегменты уходят за 50 полос; SNR ненулевой, иначе распределение не считается.
    let mut beyond = valid_blocks();
    beyond[0].snr_offsets = Some((20, 3));
    beyond[0].delta = Some((1, vec![(31, 15, 4), (31, 15, 4)]));
    assert_eq!(
        decode(&build_frame(1, &beyond, None)),
        Err(block_violation(
            0,
            BitstreamViolation::DeltaBitAllocationBeyondBands
        ))
    );
}

#[test]
fn reading_past_the_audio_area_is_an_overrun() {
    let mut skipping = valid_blocks();
    skipping[3].skip_bytes = Some(511);
    assert_eq!(
        decode(&build_frame(1, &skipping, None)),
        Err(Ac3DecodeError::BitstreamOverrun {
            section: FrameSection::AudioBlock(AudioBlockIndex::new(3))
        })
    );
}

#[test]
fn auxiliary_data_length_is_checked_against_free_bits() {
    let fits = decode(&build_frame(1, &valid_blocks(), Some(100))).unwrap();
    assert_eq!(fits.trailer.user_data_bits, Some(100));
    assert!(fits.trailer.auxiliary_bits >= 100);
    let available = fits.trailer.auxiliary_bits;
    let declared = (available + 1) as u16;
    assert_eq!(
        decode(&build_frame(1, &valid_blocks(), Some(declared))),
        Err(Ac3DecodeError::InvalidBitstream {
            section: FrameSection::FrameTrailer,
            violation: BitstreamViolation::AuxiliaryDataTooLong {
                declared_bits: available + 1,
                available_bits: available
            },
        })
    );
}

/// Кадр, где BSI пишет вызывающий тест (для полей, которых не выдаёт ffmpeg).
fn frame_with_bsi(write_bsi: impl Fn(&mut BitWriter)) -> Vec<u8> {
    let mut writer = BitWriter::default();
    writer.put(0x0B77, 16);
    writer.put(0, 16);
    writer.put(0, 2);
    writer.put(FRAME_SIZE_CODE, 6);
    write_bsi(&mut writer);
    writer.bytes.resize(FRAME_BYTES, 0);
    writer.bytes
}

#[test]
fn dual_mono_bsi_carries_second_program_metadata() {
    let frame = frame_with_bsi(|writer| {
        writer.put(8, 5);
        writer.put(4, 3); // bsmod: dialogue
        writer.put(0, 3); // acmod 1+1
        writer.flag(true); // lfeon
        // Ch1: dialnorm 0 (reserved → −31), compr 0xA5, langcod 0xFF, mixlevel 7/roomtyp 2.
        writer.put(0, 5);
        writer.flag(true);
        writer.put(0xA5, 8);
        writer.flag(true);
        writer.put(0xFF, 8);
        writer.flag(true);
        writer.put(7, 5);
        writer.put(2, 2);
        // Ch2: dialnorm 12, без остального.
        writer.put(12, 5);
        writer.put(0, 3);
        writer.put(0b10, 2); // copyright, не оригинал
        writer.flag(true);
        writer.put(0x1234, 14); // timecod1
        writer.flag(true);
        writer.put(0x0ABC, 14); // timecod2
        writer.flag(true);
        writer.put(2, 6); // addbsil → 3 байта
        writer.put(0x00C0_FFEE, 24);
    });
    let bsi = read_syncframe_info(&frame).unwrap().bsi;
    assert_eq!(bsi.audio_coding_mode, AudioCodingMode::DualMono);
    assert!(bsi.lfe_on);
    assert_eq!(bsi.program.dialogue_normalization.level_db(), -31);
    assert_eq!(bsi.program.compression_gain_word, Some(0xA5));
    assert_eq!(bsi.program.language_code, Some(0xFF));
    let production = bsi.program.production.unwrap();
    assert_eq!(
        (production.peak_mixing_level_db_spl, production.room_type),
        (87, RoomType::SmallRoomFlat)
    );
    let second = bsi.dual_mono_second_program.unwrap();
    assert_eq!(second.dialogue_normalization.level_db(), -12);
    assert_eq!(second.compression_gain_word, None);
    assert!(bsi.copyright && !bsi.original_bitstream);
    assert_eq!(
        bsi.extension,
        BsiExtension::TimeCode {
            first_half: Some(0x1234),
            second_half: Some(0x0ABC)
        }
    );
    assert_eq!(bsi.additional_bsi, [0xC0, 0xFF, 0xEE]);
    assert_eq!(bsi.center_mix_level, None);
    assert_eq!(bsi.dolby_surround_mode, None);
}

#[test]
fn reduced_rate_bitstream_ids_divide_sample_rate_and_bitrate() {
    for (bsid, rate, kbps) in [(8u32, 48_000, 128), (9, 24_000, 64), (10, 12_000, 32)] {
        let frame = frame_with_bsi(|writer| {
            writer.put(bsid, 5);
            writer.put(0, 3);
            writer.put(1, 3);
            writer.put(0, 1 + 5 + 3 + 2 + 3);
        });
        let info = read_syncframe_info(&frame).unwrap();
        assert_eq!(
            (
                info.sample_rate_hz,
                info.bitrate_kbps,
                info.frame_length_bytes
            ),
            (rate, kbps, FRAME_BYTES),
            "bsid {bsid}"
        );
    }
}
