//! Access unit-ы и синтаксис, которых нет у кодера ffmpeg: собранные побитно
//! кадры (`builder`) + реальные кадры фикстур. Проверяет состав программы
//! (Annex E §2.3.1.2, §3.8), позиции `chanmap` (Таблица E2.5), пониженные
//! частоты (Таблица E2.3), 1/2 блока, синтаксис AHT/SPX и типизированные ошибки.

use super::builder::{FrameSpec, build_frame, refresh_crc};
use super::support::{EnhancedHarness, eac3_fixture, quiet_config, split_frames};
use crate::config::CrcPolicy;
use crate::error::{Ac3DecodeError, AccessUnitViolation, BitstreamViolation, FrameSection};
use crate::output::ChannelPosition::{
    Center, Left, LeftRearSurround, LeftSurround, LowFrequencyEffects, Right, RightRearSurround,
    RightSurround, VerticalHeightCenter,
};
use crate::spectral::dither::DitherMode;
use crate::syntax::eac3::access_unit::SubstreamHeader;
use crate::syntax::eac3::inspection::inspect_access_unit;
use crate::tests::support::fixture as ac3_fixture;
use crate::tests::support::split_frames as split_ac3_frames;

/// chanmap: биты 3, 4, 6 (Ls, Rs, пара Lrs/Rrs) — 7.1 поверх 5.1 (§2.3.1.8, пример 2).
const SURROUND_BACK_PAIR_MAP: u16 = 0b0001_1010_0000_0000;

fn concat(frames: &[&[u8]]) -> Vec<u8> {
    frames
        .iter()
        .flat_map(|frame| frame.iter().copied())
        .collect()
}

fn inspect(
    unit: &[u8],
) -> Result<crate::syntax::eac3::inspection::AccessUnitInspection, Ac3DecodeError> {
    inspect_access_unit(unit, quiet_config(CrcPolicy::Verify))
}

#[test]
fn silent_frames_of_every_shape_parse_with_exact_bit_accounting() {
    let mut shapes = Vec::new();
    for acmod in 0..8 {
        for lfe_on in [false, true] {
            shapes.push(FrameSpec::independent(acmod, lfe_on));
            shapes.push(FrameSpec {
                aht: true,
                ..FrameSpec::independent(acmod, lfe_on)
            });
            shapes.push(FrameSpec {
                spx: true,
                ..FrameSpec::independent(acmod, lfe_on)
            });
            shapes.push(FrameSpec {
                second_code: 0,
                ..FrameSpec::independent(acmod, lfe_on)
            });
            shapes.push(FrameSpec {
                second_code: 1,
                snr_strategy: 2,
                ..FrameSpec::independent(acmod, lfe_on)
            });
            shapes.push(FrameSpec {
                snr_strategy: 1,
                ..FrameSpec::dependent(acmod, lfe_on, None)
            });
        }
    }
    for spec in shapes {
        let frame = build_frame(&spec);
        let mut harness = EnhancedHarness::new(DitherMode::default());
        let decoded = harness
            .decode(&frame, CrcPolicy::Verify)
            .unwrap_or_else(|error| panic!("{spec:?}: {error}"));
        let blocks = decoded.info.audio_blocks;
        assert_eq!(blocks, [1, 2, 3, 6][spec.second_code as usize], "{spec:?}");
        assert_eq!(harness.usage.audio_blocks, blocks);
        assert_eq!(
            harness.usage.spectral_extension_blocks,
            if spec.spx { blocks } else { 0 },
            "{spec:?}"
        );
        let aht_sets = decoded.info.bsi.full_bandwidth_channels() + usize::from(spec.lfe_on);
        assert_eq!(
            harness.usage.aht_channels,
            if spec.aht { aht_sets } else { 0 },
            "{spec:?}"
        );
        assert_eq!(decoded.info.audio_frame.adaptive_hybrid_transform, spec.aht);
        // Всё, что не аудио, — нули; хвост ровно 18 бит.
        let trailer = decoded.trailer;
        assert_eq!(
            trailer.audio_end_bits + trailer.auxiliary_bits + 18,
            frame.len() * 8,
            "{spec:?}"
        );
        // Полоса канала: chbwcod 0 → 73, а со SPX — конец расширения (sub-band 7 → бин 109).
        let first = &harness.blocks[0].full_bandwidth[0];
        let expected_end = if spec.spx { 109 } else { 73 };
        assert_eq!(first.end_bin, expected_end, "{spec:?}");
        // dithflage = 0 → dithflag = 1 (Таблица E1.4): все мантиссы без бит —
        // шум dither-а ниже начала SPX/endmant, тишина выше (кроме SPX-копии шума).
        let dithered = first.coefficients[..73]
            .iter()
            .filter(|&&value| value != 0.0)
            .count();
        assert!(dithered > 60, "{spec:?}: {dithered}");
    }
}

/// Пониженные частоты (Таблица E2.3): 24/22,05/16 кГц, всегда 6 блоков;
/// `fscod2 = 3` зарезервирован.
#[test]
fn reduced_sample_rates_halve_the_nominal_rates() {
    for (code, rate) in [(0, 24_000), (1, 22_050), (2, 16_000)] {
        let frame = build_frame(&FrameSpec {
            fscod: 3,
            second_code: code,
            ..FrameSpec::independent(2, false)
        });
        let inspection = inspect(&frame).unwrap();
        let SubstreamHeader::Enhanced(info) = &inspection.substreams[0].header else {
            panic!()
        };
        assert_eq!(
            (
                info.sample_rate_hz,
                info.audio_blocks,
                info.reduced_sample_rate
            ),
            (rate, 6, true)
        );
        // Без sourcefscod (только при fscod < 3) и с теми же 6 блоками.
        assert_eq!(info.samples_per_channel, 1536);
    }
    let reserved = build_frame(&FrameSpec {
        fscod: 3,
        second_code: 3,
        ..FrameSpec::independent(2, false)
    });
    assert_eq!(
        inspect(&reserved),
        Err(Ac3DecodeError::ReservedSampleRateCode)
    );
}

#[test]
fn reserved_stream_type_and_snr_strategy_are_typed_errors() {
    let stream_type = build_frame(&FrameSpec {
        stream_type: 3,
        ..FrameSpec::independent(2, false)
    });
    assert_eq!(
        inspect(&stream_type),
        Err(Ac3DecodeError::InvalidBitstream {
            section: FrameSection::BitStreamInformation,
            violation: BitstreamViolation::ReservedStreamType,
        })
    );
    let snr = build_frame(&FrameSpec {
        snr_strategy: 3,
        ..FrameSpec::independent(2, false)
    });
    assert_eq!(
        inspect(&snr),
        Err(Ac3DecodeError::InvalidBitstream {
            section: FrameSection::AudioFrame,
            violation: BitstreamViolation::ReservedSnrOffsetStrategy,
        })
    );
    let mut future = build_frame(&FrameSpec::independent(2, false));
    future[5] = (future[5] & 0x07) | (17 << 3);
    refresh_crc(&mut future);
    assert_eq!(
        inspect(&future),
        Err(Ac3DecodeError::UnsupportedBitstreamId { bitstream_id: 17 })
    );
}

/// Blu-ray 7.1: ядро AC-3 5.1 + зависимый E-AC-3 2/2 с `chanmap` Ls, Rs, Lrs, Rrs.
#[test]
fn ac3_core_with_dependent_substream_reports_both_layouts() {
    let core = split_ac3_frames(ac3_fixture("5p1-48k-640k-annexd.ac3"))[1];
    let dependent = build_frame(&FrameSpec::dependent(
        6,
        false,
        Some(SURROUND_BACK_PAIR_MAP),
    ));
    let inspection = inspect(&concat(&[core, &dependent])).unwrap();
    assert_eq!(inspection.substreams.len(), 2);
    assert!(matches!(
        inspection.substreams[0].header,
        SubstreamHeader::Ac3(_)
    ));
    assert_eq!(
        inspection.substreams[0].channels,
        [
            Left,
            Center,
            Right,
            LeftSurround,
            RightSurround,
            LowFrequencyEffects
        ]
    );
    assert_eq!(
        inspection.substreams[1].channels,
        [
            LeftSurround,
            RightSurround,
            LeftRearSurround,
            RightRearSurround
        ]
    );
    assert_eq!(inspection.skipped_substreams, 0);
    // Полоса: тихий зависимый кадр — chbwcod 0 → 73 бина у каждого канала.
    assert_eq!(inspection.substreams[1].bandwidth_bins, [73; 4]);
    assert_eq!(inspection.substreams[0].bandwidth_bins.len(), 6);
}

/// Зависимый без `chanmap` (1/0 + LFE) заменяет C и LFE (§3.8.2) — позиции из acmod.
#[test]
fn dependent_without_channel_map_takes_positions_from_acmod() {
    let independent = build_frame(&FrameSpec::independent(7, true));
    let dependent = build_frame(&FrameSpec::dependent(1, true, None));
    let inspection = inspect(&concat(&[&independent, &dependent])).unwrap();
    assert_eq!(
        inspection.substreams[1].channels,
        [Center, LowFrequencyEffects]
    );
    // chanmap с Vhc (бит 12) и LFE (бит 15) у 1/0 + LFE.
    let height = build_frame(&FrameSpec::dependent(1, true, Some(0b0000_0000_0000_1001)));
    let inspection = inspect(&concat(&[&independent, &height])).unwrap();
    assert_eq!(
        inspection.substreams[1].channels,
        [VerticalHeightCenter, LowFrequencyEffects]
    );
}

/// Другая программа (substreamid 1 + её зависимый) пропускается целиком.
#[test]
fn other_independent_programs_are_skipped() {
    let main = split_frames(eac3_fixture("stereo-48k-96k.eac3"))[0];
    let second_program = build_frame(&FrameSpec {
        substream_id: 1,
        ..FrameSpec::independent(1, false)
    });
    let second_dependent = build_frame(&FrameSpec::dependent(1, false, None));
    let main_dependent = build_frame(&FrameSpec::dependent(2, false, None));
    let unit = concat(&[main, &main_dependent, &second_program, &second_dependent]);
    let inspection = inspect(&unit).unwrap();
    assert_eq!(inspection.substreams.len(), 2);
    assert_eq!(inspection.skipped_substreams, 2);
    assert_eq!(inspection.substreams[0].channels, [Left, Right]);
}

#[test]
fn access_unit_composition_errors_are_typed() {
    let independent = build_frame(&FrameSpec::independent(2, false));
    let dependent = build_frame(&FrameSpec::dependent(2, false, None));
    let violation = |unit: &[u8]| match inspect(unit) {
        Err(Ac3DecodeError::InvalidAccessUnit(violation)) => violation,
        other => panic!("{other:?}"),
    };
    assert_eq!(
        violation(&dependent),
        AccessUnitViolation::DependentSubstreamWithoutIndependent
    );
    assert_eq!(
        violation(&concat(&[&independent, &independent])),
        AccessUnitViolation::RepeatedIndependentSubstream
    );
    let program_one = build_frame(&FrameSpec {
        substream_id: 1,
        ..FrameSpec::independent(2, false)
    });
    assert_eq!(
        violation(&program_one),
        AccessUnitViolation::MissingIndependentSubstreamZero
    );
    let many: Vec<&[u8]> = std::iter::once(independent.as_slice())
        .chain(std::iter::repeat_n(dependent.as_slice(), 9))
        .collect();
    assert_eq!(
        violation(&concat(&many)),
        AccessUnitViolation::TooManyDependentSubstreams
    );
    let eight: Vec<&[u8]> = std::iter::once(independent.as_slice())
        .chain(std::iter::repeat_n(dependent.as_slice(), 8))
        .collect();
    assert_eq!(inspect(&concat(&eight)).unwrap().substreams.len(), 9);
    let two_blocks = build_frame(&FrameSpec {
        second_code: 1,
        ..FrameSpec::dependent(2, false, None)
    });
    assert_eq!(
        violation(&concat(&[&independent, &two_blocks])),
        AccessUnitViolation::DependentSubstreamFormatMismatch
    );
    let other_rate = build_frame(&FrameSpec {
        fscod: 1,
        ..FrameSpec::dependent(2, false, None)
    });
    assert_eq!(
        violation(&concat(&[&independent, &other_rate])),
        AccessUnitViolation::DependentSubstreamFormatMismatch
    );
    let converted = build_frame(&FrameSpec {
        stream_type: 2,
        ..FrameSpec::independent(2, false)
    });
    assert_eq!(inspect(&converted).unwrap().substreams.len(), 1);
    assert_eq!(
        violation(&concat(&[&converted, &dependent])),
        AccessUnitViolation::DependentSubstreamOfConvertedStream
    );
    let core = split_ac3_frames(ac3_fixture("stereo-48k-192k.ac3"))[0];
    assert_eq!(
        violation(&concat(&[&independent, core])),
        AccessUnitViolation::MisplacedAc3Syncframe
    );
}

/// `chanmap` обязан описывать ровно `nfchans + lfeon` каналов (§2.3.1.8).
#[test]
fn channel_map_must_match_coded_channel_count() {
    let independent = build_frame(&FrameSpec::independent(7, true));
    let wrong = build_frame(&FrameSpec::dependent(
        2,
        false,
        Some(SURROUND_BACK_PAIR_MAP),
    ));
    assert_eq!(
        inspect(&concat(&[&independent, &wrong])),
        Err(Ac3DecodeError::InvalidBitstream {
            section: FrameSection::BitStreamInformation,
            violation: BitstreamViolation::ChannelMapMismatch {
                mapped_channels: 4,
                coded_channels: 2
            },
        })
    );
}

/// Тихий кадр со SPX: шум, смешанный с нулевой копией, масштабируется RMS
/// копии (= 0) — расширение обязано остаться нулём, а `end_bin` — дойти до
/// конца SPX (sub-band 7, бин 109, при `spxendf = 2`).
#[test]
fn spectral_extension_of_silence_stays_silent() {
    let frame = build_frame(&FrameSpec {
        spx: true,
        ..FrameSpec::independent(2, false)
    });
    assert_eq!(
        inspect(&frame).unwrap().substreams[0].bandwidth_bins,
        [109, 109]
    );
    let mut harness = EnhancedHarness::new(DitherMode::Disabled);
    harness.decode(&frame, CrcPolicy::Verify).unwrap();
    for block in harness.blocks.iter() {
        for spectrum in &block.full_bandwidth[..2] {
            assert_eq!(spectrum.end_bin, 109);
            assert!(spectrum.coefficients.iter().all(|&value| value == 0.0));
        }
        // Rematrixing E-AC-3 применён до SPX: синтезу передавать нечего.
        assert_eq!((block.rematrix, block.coupling_begin_subband), (None, None));
    }
}

/// `reset` разборщика возвращает генератор шума SPX к зерну: кадр со SPX его
/// продвигает (шум берётся на каждый синтезируемый бин), сброс — возвращает.
#[test]
fn parser_reset_returns_spx_noise_to_its_seed() {
    use crate::spectral::dither::DitherGenerator;
    use crate::syntax::eac3::access_unit::EnhancedAccessUnitParser;
    let frame = build_frame(&FrameSpec {
        spx: true,
        ..FrameSpec::independent(2, false)
    });
    let mut parser = EnhancedAccessUnitParser::new(CrcPolicy::Verify);
    let mut dither = DitherGenerator::new(DitherMode::Disabled);
    assert!(parser.spx_noise_is_at_seed());
    parser.parse(&frame, &mut dither).unwrap();
    assert!(!parser.spx_noise_is_at_seed());
    parser.reset();
    assert!(parser.spx_noise_is_at_seed());
}
