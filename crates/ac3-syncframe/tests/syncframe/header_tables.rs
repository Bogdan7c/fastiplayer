//! Заголовки: таблицы A/52, диапазоны bsid, E-AC-3 поля, обрезанные данные.

use ac3_syncframe::{
    AudioCodingMode, DependentChannelMap, Eac3StreamType, LowFrequencyEffects, SyncframeCodec,
    SyncframeHeaderError, SyncframeLayout, parse_syncframe_header,
};

use crate::support::{Eac3Frame, ac3_header};

/// A/52 таблица 5.18: слов (16 бит) на syncframe для 32 / 44.1 (чётный) /
/// 44.1 (нечётный) / 48 кГц, по строкам `frmsizecod / 2`. Переписано из
/// спецификации независимо от формулы в коде.
const AC3_WORDS_TABLE: [[usize; 4]; 19] = [
    [96, 69, 70, 64],
    [120, 87, 88, 80],
    [144, 104, 105, 96],
    [168, 121, 122, 112],
    [192, 139, 140, 128],
    [240, 174, 175, 160],
    [288, 208, 209, 192],
    [336, 243, 244, 224],
    [384, 278, 279, 256],
    [480, 348, 349, 320],
    [576, 417, 418, 384],
    [672, 487, 488, 448],
    [768, 557, 558, 512],
    [960, 696, 697, 640],
    [1152, 835, 836, 768],
    [1344, 975, 976, 896],
    [1536, 1114, 1115, 1024],
    [1728, 1253, 1254, 1152],
    [1920, 1393, 1394, 1280],
];

#[test]
fn every_ac3_frame_size_code_and_sample_rate_matches_spec_table() {
    for frmsizecod in 0..38_u32 {
        let row = AC3_WORDS_TABLE[(frmsizecod / 2) as usize];
        let odd = frmsizecod % 2 == 1;
        let cases = [
            (0, 48_000, row[3]),
            (1, 44_100, if odd { row[2] } else { row[1] }),
            (2, 32_000, row[0]),
        ];
        for (fscod, sample_rate, words) in cases {
            let header = parse_syncframe_header(&ac3_header(fscod, frmsizecod, 8, 2, false))
                .unwrap_or_else(|error| panic!("fscod {fscod} frmsizecod {frmsizecod}: {error}"));
            assert_eq!(header.codec(), SyncframeCodec::Ac3);
            assert_eq!(header.sample_rate_hz(), sample_rate);
            assert_eq!(
                header.frame_length_bytes(),
                words * 2,
                "fscod {fscod} frmsizecod {frmsizecod}"
            );
            assert_eq!(header.samples_per_frame(), 1536);
        }
    }
}

#[test]
fn reserved_ac3_frame_size_codes_and_sample_rate_are_rejected() {
    for frmsizecod in 38..64_u32 {
        assert_eq!(
            parse_syncframe_header(&ac3_header(0, frmsizecod, 8, 2, false)),
            Err(SyncframeHeaderError::ReservedFrameSizeCode {
                frame_size_code: frmsizecod as u8
            })
        );
    }
    assert_eq!(
        parse_syncframe_header(&ac3_header(3, 10, 8, 2, false)),
        Err(SyncframeHeaderError::ReservedSampleRateCode)
    );
}

#[test]
fn bitstream_id_selects_codec_and_rejects_reserved_ranges() {
    for bsid in 0..=10 {
        let header = parse_syncframe_header(&ac3_header(0, 8, bsid, 2, false)).expect("AC-3 bsid");
        assert_eq!(header.codec(), SyncframeCodec::Ac3);
        assert_eq!(u32::from(header.bitstream_id()), bsid);
    }
    for bsid in 11..=16 {
        let frame = Eac3Frame {
            bsid,
            ..Eac3Frame::independent_5p1()
        };
        let header = parse_syncframe_header(&frame.encode()).expect("E-AC-3 bsid");
        assert_eq!(header.codec(), SyncframeCodec::EnhancedAc3);
    }
    for bsid in 17..32 {
        assert_eq!(
            parse_syncframe_header(&ac3_header(0, 8, bsid, 2, false)),
            Err(SyncframeHeaderError::UnsupportedBitstreamId {
                bitstream_id: bsid as u8
            }),
            "bsid {bsid}"
        );
    }
}

/// bsid 9/10 — AC-3 пониженной частоты: тот же синтаксис и длина кадра (по
/// таблице базовой `fscod`), частота `fs >> 1` / `fs >> 2`, 1536 сэмплов.
#[test]
fn reduced_rate_ac3_bitstream_ids_shift_sample_rate_but_keep_frame_bytes() {
    let cases = [
        (0, 9, 24_000),
        (0, 10, 12_000),
        (1, 9, 22_050),
        (1, 10, 11_025),
        (2, 9, 16_000),
        (2, 10, 8_000),
    ];
    for (fscod, bsid, expected_rate) in cases {
        let full_rate = parse_syncframe_header(&ac3_header(fscod, 8, 8, 2, false)).expect("bsid 8");
        let header =
            parse_syncframe_header(&ac3_header(fscod, 8, bsid, 2, false)).expect("reduced rate");
        assert_eq!(header.codec(), SyncframeCodec::Ac3, "bsid {bsid}");
        assert_eq!(
            header.sample_rate_hz(),
            expected_rate,
            "fscod {fscod} bsid {bsid}"
        );
        assert_eq!(header.frame_length_bytes(), full_rate.frame_length_bytes());
        assert_eq!(header.samples_per_frame(), 1_536);
    }
}

/// Поля cmixlev/surmixlev/dsurmod переменной длины: если парсер пропустит
/// их неверно, `lfeon` прочитается не из того бита.
#[test]
fn ac3_coding_modes_skip_optional_mix_fields_before_lfe_flag() {
    let expected_full_bandwidth = [2, 1, 2, 3, 3, 4, 4, 5];
    for acmod in 0..8_u32 {
        for lfeon in [false, true] {
            let header =
                parse_syncframe_header(&ac3_header(0, 8, 8, acmod, lfeon)).expect("AC-3 header");
            assert_eq!(u32::from(header.audio_coding_mode().acmod()), acmod);
            let lfe = if lfeon {
                LowFrequencyEffects::Present
            } else {
                LowFrequencyEffects::Absent
            };
            assert_eq!(header.low_frequency_effects(), lfe, "acmod {acmod}");
            assert_eq!(
                header.channel_count(),
                expected_full_bandwidth[acmod as usize] + u32::from(lfeon)
            );
        }
    }
}

#[test]
fn eac3_sample_rates_and_block_counts_follow_fscod_and_numblkscod() {
    let blocks_by_numblkscod = [1, 2, 3, 6];
    for (fscod, sample_rate) in [(0, 48_000), (1, 44_100), (2, 32_000)] {
        for numblkscod in 0..4_u32 {
            let frame = Eac3Frame {
                fscod,
                numblkscod_or_fscod2: numblkscod,
                ..Eac3Frame::independent_5p1()
            };
            let header = parse_syncframe_header(&frame.encode()).expect("E-AC-3 header");
            assert_eq!(header.sample_rate_hz(), sample_rate);
            assert_eq!(
                header.samples_per_frame(),
                blocks_by_numblkscod[numblkscod as usize] * 256
            );
        }
    }
}

#[test]
fn eac3_reduced_sample_rates_always_have_six_blocks() {
    for (fscod2, sample_rate) in [(0, 24_000), (1, 22_050), (2, 16_000)] {
        let frame = Eac3Frame {
            fscod: 3,
            numblkscod_or_fscod2: fscod2,
            ..Eac3Frame::independent_5p1()
        };
        let header = parse_syncframe_header(&frame.encode()).expect("reduced rate");
        assert_eq!(header.sample_rate_hz(), sample_rate);
        assert_eq!(header.samples_per_frame(), 1536);
        // Остальные поля читаются со сдвигом на fscod2 правильно.
        assert_eq!(
            header.audio_coding_mode(),
            AudioCodingMode::ThreeFrontTwoSurround
        );
        assert_eq!(header.channel_count(), 6);
    }
    let reserved = Eac3Frame {
        fscod: 3,
        numblkscod_or_fscod2: 3,
        ..Eac3Frame::independent_5p1()
    };
    assert_eq!(
        parse_syncframe_header(&reserved.encode()),
        Err(SyncframeHeaderError::ReservedSampleRateCode)
    );
}

#[test]
fn eac3_stream_types_and_substream_ids_are_exposed() {
    let types = [
        (0, Eac3StreamType::Independent),
        (1, Eac3StreamType::Dependent),
        (2, Eac3StreamType::Ac3Convertible),
    ];
    for (raw_type, stream_type) in types {
        for substream_id in 0..8_u32 {
            let frame = Eac3Frame {
                stream_type: raw_type,
                substream_id,
                ..Eac3Frame::independent_5p1()
            };
            let header = parse_syncframe_header(&frame.encode()).expect("E-AC-3 header");
            let SyncframeLayout::EnhancedAc3(substream) = header.layout() else {
                panic!("E-AC-3 layout expected");
            };
            assert_eq!(substream.stream_type(), stream_type);
            assert_eq!(u32::from(substream.substream_id().value()), substream_id);
            assert_eq!(header.frame_length_bytes(), 64);
            let opens_access_unit = raw_type != 1 && substream_id == 0;
            assert_eq!(header.starts_access_unit(), opens_access_unit);
        }
    }
    let reserved = Eac3Frame {
        stream_type: 3,
        ..Eac3Frame::independent_5p1()
    };
    assert_eq!(
        parse_syncframe_header(&reserved.encode()),
        Err(SyncframeHeaderError::ReservedStreamType)
    );
}

#[test]
fn dependent_channel_map_is_read_after_dialnorm_fields() {
    let explicit = parse_syncframe_header(&Eac3Frame::dependent_back_pair().encode())
        .expect("dependent header");
    let substream = explicit.eac3_substream().expect("E-AC-3 substream");
    let DependentChannelMap::Explicit(locations) = substream.channel_map() else {
        panic!("explicit chanmap expected");
    };
    assert_eq!(locations.as_chanmap(), 1 << (15 - 6));
    assert_eq!(explicit.channel_locations().channel_count(), 2);

    // Двойное моно несёт вторые dialnorm2/compr2e перед chanmape.
    let dual_mono = Eac3Frame {
        acmod: 0,
        chanmap: Some(0b0110_0000_0000_0000),
        ..Eac3Frame::dependent_back_pair()
    };
    let header = parse_syncframe_header(&dual_mono.encode()).expect("dual mono dependent");
    assert_eq!(
        header.channel_locations().as_chanmap(),
        0b0110_0000_0000_0000
    );

    let implicit = Eac3Frame {
        chanmap: None,
        ..Eac3Frame::dependent_back_pair()
    };
    let header = parse_syncframe_header(&implicit.encode()).expect("implicit dependent");
    assert_eq!(
        header
            .eac3_substream()
            .map(|substream| substream.channel_map()),
        Some(DependentChannelMap::FromCodingMode)
    );
    assert_eq!(header.channel_locations().channel_count(), 2);
}

#[test]
fn every_truncated_prefix_reports_how_many_bytes_are_needed() {
    let frames = [
        ac3_header(0, 8, 8, 7, true),
        Eac3Frame::independent_5p1().encode(),
        Eac3Frame::dependent_back_pair().encode(),
    ];
    for frame in frames {
        let full = parse_syncframe_header(&frame).expect("full header parses");
        let mut needed_somewhere = false;
        for length in 0..frame.len() {
            match parse_syncframe_header(&frame[..length]) {
                Ok(header) => assert_eq!(header, full, "prefix {length} parsed differently"),
                Err(SyncframeHeaderError::Truncated {
                    available_bytes,
                    required_bytes,
                }) => {
                    needed_somewhere = true;
                    assert_eq!(available_bytes, length);
                    assert!(required_bytes > length);
                    // Ровно `required_bytes` байт либо дают заголовок, либо
                    // продвигают разбор дальше.
                    match parse_syncframe_header(&frame[..required_bytes]) {
                        Ok(header) => assert_eq!(header, full),
                        Err(SyncframeHeaderError::Truncated {
                            required_bytes: next,
                            ..
                        }) => assert!(next > required_bytes),
                        Err(other) => panic!("unexpected {other}"),
                    }
                }
                Err(other) => panic!("prefix {length}: unexpected {other}"),
            }
        }
        assert!(needed_somewhere);
    }
}

#[test]
fn missing_sync_word_and_header_longer_than_frame_are_rejected() {
    let mut frame = Eac3Frame::independent_5p1().encode();
    frame[1] = 0x78;
    assert_eq!(
        parse_syncframe_header(&frame),
        Err(SyncframeHeaderError::MissingSyncWord)
    );
    let mut header_source = Eac3Frame::independent_5p1().encode();
    header_source[2] &= 0xf8;
    header_source[3] = 0; // frmsiz = 0 → кадр 2 байта
    assert!(matches!(
        parse_syncframe_header(&header_source),
        Err(SyncframeHeaderError::FrameShorterThanHeader {
            frame_length_bytes: 2,
            ..
        })
    ));
}
